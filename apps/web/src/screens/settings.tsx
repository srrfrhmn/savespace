import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { type FormEvent, useEffect, useState } from "react";
import { meQuery } from "../auth/session";
import {
  currentSubscription,
  deleteAccount,
  disablePush,
  downloadExport,
  enablePush,
  type Me,
  pushSupported,
  type SettingsChange,
  testPush,
  updateSettings,
} from "../settings/data";
import { Button, describeError, Notice, Page, UserMessage } from "../ui";

const KINDS = [
  { kind: "concern", label: "When something needs your attention" },
  { kind: "anticipation", label: "Bills expected soon" },
  { kind: "awareness", label: "Changes in your spending" },
] as const;

function zones(current: string): string[] {
  let list: string[] = [];
  try {
    list = Intl.supportedValuesOf("timeZone");
  } catch {
    list = [];
  }
  return list.includes(current) ? list : [current, ...list];
}

export function Settings() {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const me = useQuery(meQuery).data;
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [subscribed, setSubscribed] = useState<boolean | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);

  useEffect(() => {
    void currentSubscription().then((s) => setSubscribed(s !== null));
  }, []);

  async function act(id: string, action: () => Promise<unknown>, done: string | null) {
    setBusy(id);
    setError(null);
    setMessage(null);
    try {
      const result = await action();
      if (result && typeof result === "object" && "notifications" in result) {
        queryClient.setQueryData(meQuery.queryKey, result as Me);
      }
      if (done) {
        setMessage(done);
      }
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(null);
    }
  }

  /** Saves a setting. The control changes at once and changes back on failure. */
  async function save(id: string, change: SettingsChange, preview: (m: Me) => Me) {
    const before = queryClient.getQueryData(meQuery.queryKey);
    queryClient.setQueryData(meQuery.queryKey, (old) => (old ? preview(old) : old));
    await act(
      id,
      async () => {
        try {
          return await updateSettings(change);
        } catch (e) {
          queryClient.setQueryData(meQuery.queryKey, before);
          throw e;
        }
      },
      "Saved.",
    );
  }

  if (!me) {
    return (
      <Page title="Settings">
        <p>Loading.</p>
      </Page>
    );
  }
  const n = me.notifications;

  function saveQuiet(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    const form = new FormData(e.currentTarget);
    void act(
      "quiet",
      () =>
        updateSettings({
          quiet_start: String(form.get("quiet_start")),
          quiet_end: String(form.get("quiet_end")),
          notify_daily_limit: Number(form.get("limit")),
        }),
      "Saved.",
    );
  }

  return (
    <Page title="Settings">
      <Notice>{message}</Notice>
      <Notice tone="problem">{error}</Notice>

      <section className="card" aria-labelledby="display-h">
        <h2 id="display-h">Display</h2>
        <label className="check">
          <input
            type="checkbox"
            checked={me.calm_mode}
            onChange={(e) =>
              save("calm", { calm_mode: e.target.checked }, (m) => ({
                ...m,
                calm_mode: e.target.checked,
              }))
            }
          />
          Calm mode: show a word instead of balances
        </label>
        <div className="field">
          <label htmlFor="tz">Time zone</label>
          <select
            id="tz"
            value={me.time_zone}
            onChange={(e) =>
              save("tz", { time_zone: e.target.value }, (m) => ({
                ...m,
                time_zone: e.target.value,
              }))
            }
          >
            {zones(me.time_zone).map((z) => (
              <option key={z} value={z}>
                {z.replace(/_/g, " ")}
              </option>
            ))}
          </select>
          <span className="meta">Days, weeks, and quiet hours use this time zone.</span>
        </div>
      </section>

      <section className="card" aria-labelledby="notify-h">
        <h2 id="notify-h">Notifications</h2>
        <p>
          SaveSpace sends at most{" "}
          {n.daily_limit === 1 ? "one notification" : `${n.daily_limit} notifications`} a day, never
          in quiet hours. If something needs your attention, it always tells you.
        </p>
        {!pushSupported() ? (
          <p className="meta">
            This browser cannot show notifications. Insights still show in the app.
          </p>
        ) : subscribed ? (
          <div className="actions">
            <Button
              variant="quiet"
              isPending={busy === "test"}
              onPress={() =>
                act(
                  "test",
                  async () => {
                    const delivered = await testPush();
                    if (delivered === 0) {
                      throw new UserMessage(
                        "The test did not arrive. Turn notifications off and on again on this device.",
                      );
                    }
                  },
                  "Test notification sent.",
                )
              }
            >
              Send a test notification
            </Button>
            <Button
              variant="quiet"
              isPending={busy === "off"}
              onPress={() =>
                act(
                  "off",
                  async () => {
                    await disablePush();
                    setSubscribed(false);
                  },
                  "Notifications are off on this device.",
                )
              }
            >
              Turn off on this device
            </Button>
          </div>
        ) : (
          <Button
            isPending={busy === "on"}
            onPress={() =>
              act(
                "on",
                async () => {
                  const allowed = await enablePush();
                  setSubscribed(allowed);
                  if (!allowed) {
                    throw new UserMessage(
                      "Notifications are blocked for this site. You can allow them in the browser settings.",
                    );
                  }
                },
                "Notifications are on for this device.",
              )
            }
          >
            Turn on notifications on this device
          </Button>
        )}

        <form className="form" onSubmit={saveQuiet}>
          <fieldset className="scale">
            <legend>Quiet hours</legend>
            <div className="inline">
              <div className="field">
                <label htmlFor="qs">From</label>
                <input
                  id="qs"
                  name="quiet_start"
                  type="time"
                  defaultValue={n.quiet_start}
                  required
                />
              </div>
              <div className="field">
                <label htmlFor="qe">To</label>
                <input id="qe" name="quiet_end" type="time" defaultValue={n.quiet_end} required />
              </div>
            </div>
          </fieldset>
          <div className="field">
            <label htmlFor="limit">Notifications a day</label>
            <select id="limit" name="limit" defaultValue={String(n.daily_limit)}>
              {[0, 1, 2, 3, 4, 5].map((v) => (
                <option key={v} value={v}>
                  {v === 0 ? "None (only when something needs attention)" : v}
                </option>
              ))}
            </select>
          </div>
          <Button type="submit" isPending={busy === "quiet"}>
            Save
          </Button>
        </form>

        <fieldset className="scale">
          <legend>Send notifications for</legend>
          {KINDS.map((k) => (
            <label key={k.kind} className="check">
              <input
                type="checkbox"
                checked={!n.muted_kinds.includes(k.kind)}
                onChange={(e) => {
                  const muted = e.target.checked
                    ? n.muted_kinds.filter((m) => m !== k.kind)
                    : [...n.muted_kinds, k.kind];
                  void save("kinds", { notify_muted_kinds: muted }, (m) => ({
                    ...m,
                    notifications: { ...m.notifications, muted_kinds: muted },
                  }));
                }}
              />
              {k.label}
            </label>
          ))}
        </fieldset>
      </section>

      <section className="card" aria-labelledby="data-h">
        <h2 id="data-h">Your data</h2>
        <p>
          Download everything SaveSpace stores about you as JSON and CSV. SaveSpace asks for your
          passkey first.
        </p>
        <Button
          variant="quiet"
          isPending={busy === "export"}
          onPress={() => act("export", downloadExport, "Your download has started.")}
        >
          Download my data
        </Button>
        <h3>Delete your account</h3>
        <p>
          This removes your bank connections at Plaid and deletes your account and all your data. It
          cannot be undone. Backups that hold encrypted copies expire within 35 days.
        </p>
        {confirmDelete ? (
          <fieldset className="confirm">
            <legend>Delete your account and all data?</legend>
            <Button
              isPending={busy === "delete"}
              onPress={() =>
                act(
                  "delete",
                  async () => {
                    await deleteAccount();
                    queryClient.clear();
                    await navigate({ to: "/deleted" });
                  },
                  null,
                )
              }
            >
              Delete everything
            </Button>
            <Button variant="quiet" onPress={() => setConfirmDelete(false)}>
              Keep my account
            </Button>
          </fieldset>
        ) : (
          <Button variant="quiet" onPress={() => setConfirmDelete(true)}>
            Delete my account
          </Button>
        )}
      </section>

      <section className="card" aria-labelledby="about-h">
        <h2 id="about-h">About SaveSpace</h2>
        <p>
          SaveSpace is not a medical service and not financial advice. If money worries feel too
          heavy, help is available.
        </p>
        <nav className="links">
          <Link to="/help">Help and support</Link>
          <Link to="/privacy">Privacy notice</Link>
          <Link to="/settings/security">Passkeys and recovery codes</Link>
          <Link to="/review">Insight review</Link>
        </nav>
      </section>

      <nav className="links">
        <Link to="/">Back to home</Link>
      </nav>
    </Page>
  );
}
