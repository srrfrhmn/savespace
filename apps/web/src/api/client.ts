import createClient from "openapi-fetch";
import type { components, paths } from "../../../../clients/ts/schema";

/** The typed API client. Types come from clients/openapi.json (pnpm gen:api). */
export const api = createClient<paths>({ baseUrl: "/v1", credentials: "same-origin" });

export type Schemas = components["schemas"];
export type ApiErrorBody = Schemas["ErrorBody"]["error"];

/** An API error with the stable `code` from the response (design doc §11.4). */
export class ApiFailure extends Error {
  readonly status: number;
  readonly code: string;
  readonly requestId: string | undefined;

  constructor(status: number, body: unknown) {
    const error = (body as { error?: Partial<ApiErrorBody> } | undefined)?.error;
    super(error?.message ?? `Request failed with status ${status}`);
    this.name = "ApiFailure";
    this.status = status;
    this.code = error?.code ?? "unknown";
    this.requestId = error?.request_id ?? undefined;
  }
}

/**
 * Returns `data`, or throws an ApiFailure. Use it with the openapi-fetch result:
 * `const me = ok(await api.GET("/me"))`.
 */
export function ok<T>(result: { data?: T; error?: unknown; response: Response }): T {
  if (result.response.ok) {
    return result.data as T;
  }
  throw new ApiFailure(result.response.status, result.error);
}

/** A new Idempotency-Key for a state-changing request (design doc §11.3). */
export function idempotencyKey(): string {
  return crypto.randomUUID();
}
