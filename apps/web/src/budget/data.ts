import { queryOptions } from "@tanstack/react-query";
import { api, ok, type Schemas } from "../api/client";

export type MonthBudget = Schemas["MonthBudget"];
export type CategoryBudget = Schemas["CategoryBudget"];
export type Calm = Schemas["CalmState"];
export type RecurringSeries = Schemas["RecurringSeries"];

/** "2026-11" for a date, in the browser's time zone. */
export function monthKey(date: Date): string {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}`;
}

/** The month before or after "YYYY-MM". */
export function shiftMonth(month: string, by: number): string {
  const [y, m] = month.split("-").map(Number);
  return monthKey(new Date(y ?? 2000, (m ?? 1) - 1 + by, 1));
}

const monthName = new Intl.DateTimeFormat("en-US", { month: "long", year: "numeric" });

/** "November 2026". */
export function monthLabel(month: string): string {
  const [y, m] = month.split("-").map(Number);
  return monthName.format(new Date(y ?? 2000, (m ?? 1) - 1, 1));
}

export function budgetQuery(month: string) {
  return queryOptions({
    queryKey: ["budget", month],
    queryFn: async () => ok(await api.GET("/budgets/{month}", { params: { path: { month } } })),
  });
}

export const recurringQuery = queryOptions({
  queryKey: ["recurring"],
  queryFn: async () => ok(await api.GET("/recurring")).data,
});

export type PlanChange = Schemas["SetPlanRequest"];

export async function setPlan(month: string, categoryId: string, change: PlanChange) {
  ok(
    await api.PUT("/budgets/{month}/categories/{id}", {
      params: { path: { month, id: categoryId } },
      body: change,
    }),
  );
}

export async function setCalmMode(calm: boolean) {
  return ok(await api.PATCH("/me", { body: { calm_mode: calm } }));
}
