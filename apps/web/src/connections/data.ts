import { queryOptions } from "@tanstack/react-query";
import { api, idempotencyKey, ok, type Schemas } from "../api/client";
import { withStepUp } from "../auth/session";
import { openPlaidLink } from "./plaid";

export type Connection = Schemas["Connection"];
export type Account = Schemas["Account"];
export type Transaction = Schemas["Transaction"];

/** Connections. While one is syncing, poll every 3 seconds (D15: no SSE yet). */
export const connectionsQuery = queryOptions({
  queryKey: ["connections"],
  queryFn: async () => ok(await api.GET("/connections")).data,
  refetchInterval: (query) =>
    query.state.data?.some((c) => c.status === "syncing") ? 3_000 : false,
});

export const accountsQuery = queryOptions({
  queryKey: ["accounts"],
  queryFn: async () => ok(await api.GET("/accounts")).data,
});

export const recentTransactionsQuery = queryOptions({
  queryKey: ["transactions", "recent"],
  queryFn: async () => ok(await api.GET("/transactions", { params: { query: { limit: 10 } } })),
});

/**
 * Connects a bank: link token (step-up), Plaid Link, then the exchange.
 * Returns false if the person closed Link.
 */
export async function connectBank(): Promise<boolean> {
  const link = await withStepUp(async () => ok(await api.POST("/link_tokens", { body: {} })));
  const result = await openPlaidLink(link.link_token);
  if (!result) {
    return false;
  }
  ok(
    await api.POST("/connections", {
      body: { public_token: result.publicToken, institution: result.institution },
      headers: { "Idempotency-Key": idempotencyKey() },
    }),
  );
  return true;
}

/** Repairs a connection in Plaid Link update mode, then syncs it. */
export async function repairConnection(id: string): Promise<boolean> {
  const link = await withStepUp(async () =>
    ok(await api.POST("/link_tokens", { body: { connection_id: id } })),
  );
  const result = await openPlaidLink(link.link_token);
  if (!result) {
    return false;
  }
  await refreshConnection(id);
  return true;
}

export async function refreshConnection(id: string): Promise<void> {
  ok(await api.POST("/connections/{id}/sync", { params: { path: { id } } }));
}

export async function removeConnection(id: string): Promise<void> {
  await withStepUp(async () => {
    ok(await api.DELETE("/connections/{id}", { params: { path: { id } } }));
  });
}
