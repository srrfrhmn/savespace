/**
 * WebAuthn helpers. The server sends options in the WebAuthn JSON form, with
 * binary fields as base64url. The browser API needs ArrayBuffers. These
 * helpers convert both ways, so they work in browsers without
 * PublicKeyCredential.parseCreationOptionsFromJSON.
 */

type Json = Record<string, unknown>;

export function base64urlToBuffer(value: string): ArrayBuffer {
  const base64 = value.replace(/-/g, "+").replace(/_/g, "/");
  const padded = base64 + "=".repeat((4 - (base64.length % 4)) % 4);
  const binary = atob(padded);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes.buffer;
}

export function bufferToBase64url(buffer: ArrayBuffer | ArrayBufferView): string {
  const bytes =
    buffer instanceof ArrayBuffer
      ? new Uint8Array(buffer)
      : new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function descriptors(list: unknown): PublicKeyCredentialDescriptor[] | undefined {
  if (!Array.isArray(list)) {
    return undefined;
  }
  return list.map((item: Json) => ({
    ...(item as unknown as PublicKeyCredentialDescriptor),
    id: base64urlToBuffer(item.id as string),
  }));
}

/** Converts server creation options (`{ publicKey: … }`) for credentials.create(). */
export function toCreationOptions(options: unknown): CredentialCreationOptions {
  const publicKey = (options as { publicKey: Json }).publicKey;
  const user = publicKey.user as Json;
  const exclude = descriptors(publicKey.excludeCredentials);
  return {
    publicKey: {
      ...(publicKey as unknown as PublicKeyCredentialCreationOptions),
      challenge: base64urlToBuffer(publicKey.challenge as string),
      user: {
        ...(user as unknown as PublicKeyCredentialUserEntity),
        id: base64urlToBuffer(user.id as string),
      },
      ...(exclude ? { excludeCredentials: exclude } : {}),
    },
  };
}

/** Converts server request options (`{ publicKey: … }`) for credentials.get(). */
export function toRequestOptions(options: unknown): CredentialRequestOptions {
  const publicKey = (options as { publicKey: Json }).publicKey;
  const allow = descriptors(publicKey.allowCredentials);
  return {
    publicKey: {
      ...(publicKey as unknown as PublicKeyCredentialRequestOptions),
      challenge: base64urlToBuffer(publicKey.challenge as string),
      ...(allow ? { allowCredentials: allow } : {}),
    },
  };
}

/** The JSON form of a new credential, for the server. */
export function registrationJson(credential: PublicKeyCredential): Json {
  const response = credential.response as AuthenticatorAttestationResponse;
  return {
    id: credential.id,
    rawId: bufferToBase64url(credential.rawId),
    type: credential.type,
    response: {
      attestationObject: bufferToBase64url(response.attestationObject),
      clientDataJSON: bufferToBase64url(response.clientDataJSON),
      transports: response.getTransports?.() ?? [],
    },
    extensions: {},
  };
}

/** The JSON form of an assertion, for the server. */
export function assertionJson(credential: PublicKeyCredential): Json {
  const response = credential.response as AuthenticatorAssertionResponse;
  return {
    id: credential.id,
    rawId: bufferToBase64url(credential.rawId),
    type: credential.type,
    response: {
      authenticatorData: bufferToBase64url(response.authenticatorData),
      clientDataJSON: bufferToBase64url(response.clientDataJSON),
      signature: bufferToBase64url(response.signature),
      userHandle: response.userHandle ? bufferToBase64url(response.userHandle) : null,
    },
    extensions: {},
  };
}

/** True if this browser supports passkeys. */
export function passkeysSupported(): boolean {
  return typeof window !== "undefined" && "PublicKeyCredential" in window;
}

/** Creates a passkey. Throws PasskeyCancelled if the person closes the dialog. */
export async function createPasskey(options: unknown): Promise<Json> {
  const credential = await callBrowser(() =>
    navigator.credentials.create(toCreationOptions(options)),
  );
  return registrationJson(credential);
}

/** Uses a passkey. Throws PasskeyCancelled if the person closes the dialog. */
export async function getPasskey(options: unknown): Promise<Json> {
  const credential = await callBrowser(() => navigator.credentials.get(toRequestOptions(options)));
  return assertionJson(credential);
}

/** The person closed the passkey dialog, or the time ran out. */
export class PasskeyCancelled extends Error {
  constructor() {
    super("The passkey request was cancelled.");
    this.name = "PasskeyCancelled";
  }
}

async function callBrowser(call: () => Promise<Credential | null>): Promise<PublicKeyCredential> {
  try {
    const credential = await call();
    if (!credential) {
      throw new PasskeyCancelled();
    }
    return credential as PublicKeyCredential;
  } catch (e) {
    if (e instanceof DOMException && (e.name === "NotAllowedError" || e.name === "AbortError")) {
      throw new PasskeyCancelled();
    }
    throw e;
  }
}
