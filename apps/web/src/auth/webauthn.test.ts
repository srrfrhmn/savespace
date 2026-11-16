import { describe, expect, it } from "vitest";
import {
  base64urlToBuffer,
  bufferToBase64url,
  toCreationOptions,
  toRequestOptions,
} from "./webauthn";

describe("base64url", () => {
  it("round-trips every byte value", () => {
    const bytes = new Uint8Array(256).map((_, i) => i);
    const encoded = bufferToBase64url(bytes);
    expect(encoded).not.toMatch(/[+/=]/);
    expect(new Uint8Array(base64urlToBuffer(encoded))).toEqual(bytes);
  });

  it("decodes lengths that need padding", () => {
    for (const text of ["a", "ab", "abc", "abcd"]) {
      const encoded = bufferToBase64url(new TextEncoder().encode(text));
      expect(new TextDecoder().decode(base64urlToBuffer(encoded))).toBe(text);
    }
  });
});

describe("options", () => {
  it("converts creation options from the server", () => {
    const options = toCreationOptions({
      publicKey: {
        challenge: "AQID",
        rp: { id: "localhost", name: "SaveSpace" },
        user: { id: "BAUG", name: "Sam", displayName: "Sam" },
        pubKeyCredParams: [{ type: "public-key", alg: -7 }],
        excludeCredentials: [{ type: "public-key", id: "Bwg" }],
        authenticatorSelection: { residentKey: "required", userVerification: "required" },
      },
    });
    const pk = options.publicKey;
    expect(new Uint8Array(pk?.challenge as ArrayBuffer)).toEqual(new Uint8Array([1, 2, 3]));
    expect(new Uint8Array(pk?.user.id as ArrayBuffer)).toEqual(new Uint8Array([4, 5, 6]));
    expect(new Uint8Array(pk?.excludeCredentials?.[0]?.id as ArrayBuffer)).toEqual(
      new Uint8Array([7, 8]),
    );
    expect(pk?.authenticatorSelection?.residentKey).toBe("required");
  });

  it("converts request options without allowCredentials", () => {
    const options = toRequestOptions({
      publicKey: { challenge: "AQID", rpId: "localhost", userVerification: "required" },
    });
    expect(options.publicKey?.allowCredentials).toBeUndefined();
    expect(options.publicKey?.rpId).toBe("localhost");
  });
});
