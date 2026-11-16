import { queryOptions } from "@tanstack/react-query";
import { ApiFailure, api, idempotencyKey, ok, type Schemas } from "../api/client";
import { createPasskey, getPasskey } from "./webauthn";

export type Me = Schemas["Me"];

/** The signed-in person, or null when signed out. */
export const meQuery = queryOptions({
  queryKey: ["me"],
  queryFn: async (): Promise<Me | null> => {
    const result = await api.GET("/me");
    if (result.response.status === 401) {
      return null;
    }
    return ok(result);
  },
  staleTime: 60_000,
});

/** Whether owner setup can start (design doc D12). */
export const setupStatusQuery = queryOptions({
  queryKey: ["auth", "setup"],
  queryFn: async () => ok(await api.GET("/auth/setup")),
});

/** Signs in with a passkey. The browser lists the passkeys for this site. */
export async function signInWithPasskey(): Promise<void> {
  const challenge = ok(await api.POST("/auth/login/start"));
  const credential = await getPasskey(challenge.options);
  ok(
    await api.POST("/auth/login/finish", {
      body: { challenge_id: challenge.challenge_id, credential },
    }),
  );
}

/** Signs in with a recovery code. */
export async function signInWithRecoveryCode(code: string): Promise<void> {
  ok(await api.POST("/auth/recover", { body: { code } }));
}

/** Creates the owner and the first passkey. Returns the recovery codes. */
export async function setUpOwner(input: {
  setupCode: string;
  displayName: string;
  passkeyName: string;
}): Promise<string[]> {
  const challenge = ok(
    await api.POST("/auth/setup/start", {
      body: { setup_code: input.setupCode, display_name: input.displayName },
    }),
  );
  const credential = await createPasskey(challenge.options);
  const result = ok(
    await api.POST("/auth/setup/finish", {
      body: {
        challenge_id: challenge.challenge_id,
        credential,
        passkey_name: input.passkeyName,
      },
    }),
  );
  return result.recovery_codes;
}

export async function signOut(): Promise<void> {
  ok(await api.POST("/auth/logout"));
}

/** Confirms with a passkey, so step-up actions work for 5 minutes (§12.1). */
export async function confirmWithPasskey(): Promise<void> {
  const challenge = ok(await api.POST("/auth/verify/start"));
  const credential = await getPasskey(challenge.options);
  ok(
    await api.POST("/auth/verify/finish", {
      body: { challenge_id: challenge.challenge_id, credential },
    }),
  );
}

/**
 * Runs a step-up action. If the server asks for a passkey check, runs the
 * check, then the action once more.
 */
export async function withStepUp<T>(action: () => Promise<T>): Promise<T> {
  try {
    return await action();
  } catch (e) {
    if (e instanceof ApiFailure && e.code === "step_up_required") {
      await confirmWithPasskey();
      return action();
    }
    throw e;
  }
}

/** Adds a passkey. Needs step-up. */
export async function addPasskey(name: string): Promise<void> {
  await withStepUp(async () => {
    const challenge = ok(
      await api.POST("/me/passkeys/start", {
        headers: { "Idempotency-Key": idempotencyKey() },
      }),
    );
    const credential = await createPasskey(challenge.options);
    ok(
      await api.POST("/me/passkeys/finish", {
        body: { challenge_id: challenge.challenge_id, credential, name },
        headers: { "Idempotency-Key": idempotencyKey() },
      }),
    );
  });
}

/** Deletes a passkey. Needs step-up. */
export async function deletePasskey(id: string): Promise<void> {
  await withStepUp(async () => {
    ok(await api.DELETE("/me/passkeys/{id}", { params: { path: { id } } }));
  });
}

/** Replaces the recovery codes. Needs step-up. */
export async function newRecoveryCodes(): Promise<string[]> {
  // A new key for each attempt: the server stores the step_up_required answer too.
  return withStepUp(async () => {
    const result = ok(
      await api.POST("/me/recovery_codes", {
        headers: { "Idempotency-Key": idempotencyKey() },
      }),
    );
    return result.recovery_codes;
  });
}
