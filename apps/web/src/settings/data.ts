import { api, ok, type Schemas } from "../api/client";
import { withStepUp } from "../auth/session";
import { UserMessage } from "../ui";

export type Me = Schemas["Me"];
export type SettingsChange = Schemas["UpdateMeRequest"];

export async function updateSettings(change: SettingsChange): Promise<Me> {
  return ok(await api.PATCH("/me", { body: change }));
}

/** True if this browser can receive Web Push. */
export function pushSupported(): boolean {
  return "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;
}

function keyBytes(base64url: string): Uint8Array<ArrayBuffer> {
  const base64 = base64url.replace(/-/g, "+").replace(/_/g, "/");
  const raw = atob(base64 + "=".repeat((4 - (base64.length % 4)) % 4));
  const bytes = new Uint8Array(new ArrayBuffer(raw.length));
  for (let i = 0; i < raw.length; i++) {
    bytes[i] = raw.charCodeAt(i);
  }
  return bytes;
}

async function registration(): Promise<ServiceWorkerRegistration> {
  await navigator.serviceWorker.register("/sw.js", { scope: "/" });
  return navigator.serviceWorker.ready;
}

/** This browser's current subscription, if any. */
export async function currentSubscription(): Promise<PushSubscription | null> {
  if (!pushSupported()) {
    return null;
  }
  const reg = await navigator.serviceWorker.getRegistration("/");
  return reg ? reg.pushManager.getSubscription() : null;
}

/**
 * Asks for permission, subscribes, and saves the subscription. Returns
 * false if the person did not allow notifications.
 */
export async function enablePush(): Promise<boolean> {
  const permission = await Notification.requestPermission();
  if (permission !== "granted") {
    return false;
  }
  const { public_key } = ok(await api.GET("/push/key"));
  const reg = await registration();
  let subscription: PushSubscription;
  try {
    subscription =
      (await reg.pushManager.getSubscription()) ??
      (await reg.pushManager.subscribe({
        userVisibleOnly: true,
        applicationServerKey: keyBytes(public_key),
      }));
  } catch {
    // For example a private window, where browsers do not support push.
    throw new UserMessage(
      "This browser could not turn on notifications. Private windows do not support them.",
    );
  }
  const json = subscription.toJSON();
  ok(
    await api.POST("/push/subscriptions", {
      body: {
        endpoint: subscription.endpoint,
        keys: { p256dh: json.keys?.p256dh ?? "", auth: json.keys?.auth ?? "" },
      },
    }),
  );
  return true;
}

export async function disablePush(): Promise<void> {
  const subscription = await currentSubscription();
  if (!subscription) {
    return;
  }
  ok(await api.DELETE("/push/subscriptions", { body: { endpoint: subscription.endpoint } }));
  await subscription.unsubscribe();
}

export async function testPush(): Promise<number> {
  return ok(await api.POST("/push/test")).delivered;
}

/** Downloads the export zip. Needs a passkey check. */
export async function downloadExport(): Promise<void> {
  const blob = await withStepUp(async () => {
    const result = await api.POST("/exports", { parseAs: "blob" });
    return ok(result) as unknown as Blob;
  });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = `savespace-export-${new Date().toISOString().slice(0, 10)}.zip`;
  document.body.append(link);
  link.click();
  link.remove();
  URL.revokeObjectURL(url);
}

/** Deletes the account and all data. Needs a passkey check. */
export async function deleteAccount(): Promise<void> {
  await disablePush().catch(() => undefined);
  await withStepUp(async () => {
    ok(await api.DELETE("/me"));
  });
}
