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

export type Review = Schemas["Review"];
export type ReviewItem = Schemas["ReviewItem"];
export type Verdict = "correct" | "wrong";

export function reviewQuery(days: number) {
  return queryOptions({
    queryKey: ["insights", "review", days],
    queryFn: async () => ok(await api.GET("/insights/review", { params: { query: { days } } })),
  });
}

export async function setReview(id: string, verdict: Verdict | null): Promise<void> {
  ok(await api.PUT("/insights/{id}/review", { params: { path: { id } }, body: { verdict } }));
}

/** The review counts after one verdict changes, for an optimistic update. */
export function withVerdict(review: Review, id: string, verdict: Verdict | null): Review {
  const data = review.data.map((i) => {
    if (i.id !== id) {
      return i;
    }
    if (verdict) {
      return { ...i, verdict, verdict_source: "review" };
    }
    // Without a review, "This is wrong" in the app still counts.
    return i.status === "wrong"
      ? { ...i, verdict: "wrong", verdict_source: "feedback" }
      : { ...i, verdict: null, verdict_source: null };
  });
  const correct = data.filter((i) => i.verdict === "correct").length;
  const wrong = data.filter((i) => i.verdict === "wrong").length;
  const reviewed = correct + wrong;
  return {
    data,
    summary: {
      ...review.summary,
      correct,
      wrong,
      rate_bp: reviewed === 0 ? null : Math.floor((correct * 10000) / reviewed),
    },
  };
}

/** "99.50%" from basis points, rounded down. */
export function percent(bp: number): string {
  return `${(Math.floor(bp) / 100).toFixed(2)}%`;
}
