import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { categoriesQuery, deleteRule, type Rule, rulesQuery } from "../transactions/data";
import { Button, describeError, Notice, Page } from "../ui";

function target(rule: Rule): string {
  const m = rule.match;
  const what = m.merchant_id
    ? (rule.merchant_name ?? "One merchant")
    : `Description contains "${m.name_contains ?? ""}"`;
  if (m.direction === "in") {
    return `${what}, money in`;
  }
  if (m.direction === "out") {
    return `${what}, money out`;
  }
  return what;
}

/** The person's categorization rules (D14). */
export function Rules() {
  const queryClient = useQueryClient();
  const rules = useQuery(rulesQuery);
  const categories = useQuery(categoriesQuery);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const byId = new Map((categories.data ?? []).map((c) => [c.id, c.name]));

  async function remove(rule: Rule) {
    setBusy(rule.id);
    setError(null);
    setMessage(null);
    try {
      await deleteRule(rule.id);
      await queryClient.invalidateQueries({ queryKey: ["rules"] });
      await queryClient.invalidateQueries({ queryKey: ["transactions"] });
      setMessage("Rule deleted. Those transactions use the automatic category again.");
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <Page title="Category rules">
      <p className="lead">
        A rule puts a merchant in a category every time, for past and new transactions. A category
        you set on one transaction always stays.
      </p>
      <Notice>{message}</Notice>
      <Notice tone="problem">{error}</Notice>
      {rules.isError ? <Notice tone="problem">{describeError(rules.error)}</Notice> : null}
      {rules.data?.length === 0 ? (
        <section className="card">
          <p>
            No rules yet. When you change the category of a transaction, SaveSpace asks if you want
            one.
          </p>
        </section>
      ) : null}
      {rules.data && rules.data.length > 0 ? (
        <section className="card" aria-label="Rules">
          <ul className="list">
            {rules.data.map((r) => (
              <li key={r.id}>
                <span>
                  <strong>{target(r)}</strong>
                  <span className="meta">
                    {byId.get(r.category_id) ?? "Other"}. {r.transactions}{" "}
                    {r.transactions === 1 ? "transaction" : "transactions"} now.
                  </span>
                </span>
                <Button variant="quiet" isPending={busy === r.id} onPress={() => remove(r)}>
                  Delete
                </Button>
              </li>
            ))}
          </ul>
        </section>
      ) : null}
      <nav className="links">
        <Link to="/transactions">Transactions</Link>
        <Link to="/">Back to home</Link>
      </nav>
    </Page>
  );
}
