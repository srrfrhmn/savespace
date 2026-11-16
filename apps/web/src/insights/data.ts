import { queryOptions } from "@tanstack/react-query";
import { api, ok, type Schemas } from "../api/client";

export type Insight = Schemas["Insight"];
export type Reflection = Schemas["Reflection"];
export type CheckIn = Schemas["CheckIn"];

export const homeInsightsQuery = queryOptions({
  queryKey: ["insights", "home"],
  queryFn: async () => ok(await api.GET("/insights")).data,
});

export function transactionInsightsQuery(transactionId: string) {
  return queryOptions({
    queryKey: ["insights", "transaction", transactionId],
    queryFn: async () =>
      ok(await api.GET("/insights", { params: { query: { transaction_id: transactionId } } })).data,
  });
}

export async function sendFeedback(
  id: string,
  kind: "wrong" | "not_helpful",
  reason?: string,
): Promise<boolean> {
  const body = reason ? { kind, reason } : { kind };
  return ok(await api.POST("/insights/{id}/feedback", { params: { path: { id } }, body })).muted;
}

export const mutesQuery = queryOptions({
  queryKey: ["insights", "mutes"],
  queryFn: async () => ok(await api.GET("/insights/mutes")).data,
});

export async function unmute(ruleId: string) {
  ok(await api.DELETE("/insights/mutes/{rule_id}", { params: { path: { rule_id: ruleId } } }));
}

/** "2026-11-16" for a date, in the browser's time zone. */
export function isoDate(date: Date): string {
  const m = String(date.getMonth() + 1).padStart(2, "0");
  const d = String(date.getDate()).padStart(2, "0");
  return `${date.getFullYear()}-${m}-${d}`;
}

export function reflectionQuery(week: string) {
  return queryOptions({
    queryKey: ["reflection", week],
    queryFn: async () => ok(await api.GET("/reflections/{week}", { params: { path: { week } } })),
  });
}

export const checkInsQuery = queryOptions({
  queryKey: ["check_ins"],
  queryFn: async () => ok(await api.GET("/check_ins")).data,
});

export async function saveCheckIn(score: number, note: string): Promise<CheckIn> {
  const trimmed = note.trim();
  return ok(await api.POST("/check_ins", { body: trimmed ? { score, note: trimmed } : { score } }));
}
