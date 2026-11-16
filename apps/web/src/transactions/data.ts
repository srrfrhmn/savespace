import { infiniteQueryOptions, queryOptions } from "@tanstack/react-query";
import { api, idempotencyKey, ok, type Schemas } from "../api/client";

export type Transaction = Schemas["Transaction"];
export type Category = Schemas["Category"];
export type Rule = Schemas["Rule"];

/** The list filters. Empty strings mean "no filter". */
export type Filters = {
  q: string;
  account: string;
  category: string;
  from: string;
  to: string;
  includeHidden: boolean;
};

export const noFilters: Filters = {
  q: "",
  account: "",
  category: "",
  from: "",
  to: "",
  includeHidden: false,
};

export const categoriesQuery = queryOptions({
  queryKey: ["categories"],
  queryFn: async () => ok(await api.GET("/categories")).data,
  staleTime: 5 * 60_000,
});

export const rulesQuery = queryOptions({
  queryKey: ["rules"],
  queryFn: async () => ok(await api.GET("/rules")).data,
});

const PAGE = 50;

/** Transactions with filters, 50 per page, newest first. */
export function transactionsQuery(filters: Filters) {
  return infiniteQueryOptions({
    queryKey: ["transactions", "list", filters],
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam }) =>
      ok(
        await api.GET("/transactions", {
          params: {
            query: {
              limit: PAGE,
              ...(pageParam ? { starting_after: pageParam } : {}),
              ...(filters.q.trim() ? { q: filters.q.trim() } : {}),
              ...(filters.account ? { account_id: filters.account } : {}),
              ...(filters.category ? { category: filters.category } : {}),
              ...(filters.from ? { from: filters.from } : {}),
              ...(filters.to ? { to: filters.to } : {}),
              ...(filters.includeHidden ? { include_hidden: true } : {}),
            },
          },
        }),
      ),
    getNextPageParam: (last) => (last.has_more ? last.data.at(-1)?.id : undefined),
  });
}

export type TransactionEdit = Schemas["UpdateTransactionRequest"];

export async function updateTransaction(id: string, edit: TransactionEdit): Promise<Transaction> {
  return ok(await api.PATCH("/transactions/{id}", { params: { path: { id } }, body: edit }));
}

export async function setSplits(
  id: string,
  splits: { category_id: string; amount_minor: number }[],
): Promise<Transaction> {
  return ok(
    await api.PUT("/transactions/{id}/splits", { params: { path: { id } }, body: { splits } }),
  );
}

/** "Always put this merchant in this category." Applies to past transactions too. */
export async function createMerchantRule(merchantId: string, categoryId: string): Promise<Rule> {
  return ok(
    await api.POST("/rules", {
      body: { match: { merchant_id: merchantId }, category_id: categoryId },
      headers: { "Idempotency-Key": idempotencyKey() },
    }),
  );
}

export async function deleteRule(id: string): Promise<void> {
  ok(await api.DELETE("/rules/{id}", { params: { path: { id } } }));
}
