import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { type FormEvent, useId, useState } from "react";
import { meQuery } from "../auth/session";
import { CalmCard } from "../budget/calm";
import {
  budgetQuery,
  type CategoryBudget,
  monthKey,
  monthLabel,
  setPlan,
  shiftMonth,
} from "../budget/data";
import { money, parseAmount } from "../format";
import { categoriesQuery } from "../transactions/data";
import { Button, describeError, Notice, Page } from "../ui";

const BUCKETS = [
  { kind: "fixed", title: "Fixed", note: "Bills that stay about the same each month." },
  { kind: "flex", title: "Flexible", note: "Day-to-day spending you can change." },
  {
    kind: "non_monthly",
    title: "Non-monthly",
    note: "Yearly or occasional costs. SaveSpace sets aside 1/12 of the yearly plan each month.",
  },
] as const;

export function Budget() {
  const current = monthKey(new Date());
  const [month, setMonth] = useState(current);
  const budget = useQuery(budgetQuery(month));
  const categories = useQuery(categoriesQuery);
  const me = useQuery(meQuery).data;
  const [editing, setEditing] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const names = new Map((categories.data ?? []).map((c) => [c.id, c.name]));
  const b = budget.data;

  return (
    <Page title="Budget">
      <nav className="month-nav" aria-label="Month">
        <Button variant="quiet" onPress={() => setMonth(shiftMonth(month, -1))}>
          Previous month
        </Button>
        <h2 aria-live="polite">{monthLabel(month)}</h2>
        <Button
          variant="quiet"
          isDisabled={month >= current}
          onPress={() => setMonth(shiftMonth(month, 1))}
        >
          Next month
        </Button>
      </nav>

      {budget.isError ? <Notice tone="problem">{describeError(budget.error)}</Notice> : null}
      {budget.isPending ? <p>Loading.</p> : null}
      <Notice>{message}</Notice>

      {b?.calm ? (
        <CalmCard calm={b.calm} currency={b.currency} calmMode={me?.calm_mode ?? false} />
      ) : null}

      {b ? (
        <p>
          Money in this month: {money(b.income_minor, b.currency)}.
          {b.uncategorized_minor > 0
            ? ` Spent without a category: ${money(b.uncategorized_minor, b.currency)}. You can set one on the Transactions page.`
            : ""}
        </p>
      ) : null}

      {b
        ? BUCKETS.map((bucket) => {
            const rows = b.categories.filter((c) => c.kind === bucket.kind);
            const totals = b[bucket.kind];
            return (
              <section key={bucket.kind} className="card" aria-labelledby={`${bucket.kind}-h`}>
                <h2 id={`${bucket.kind}-h`}>{bucket.title}</h2>
                <p className="meta">{bucket.note}</p>
                <p>
                  {bucket.kind === "non_monthly"
                    ? `Set aside so far this year: ${money(totals.planned_minor, b.currency)}. This year: ${spentText(totals.spent_minor, b.currency)}.`
                    : `Planned ${money(totals.planned_minor, b.currency)}. ${spentText(totals.spent_minor, b.currency)}.`}
                </p>
                <ul className="budget-rows">
                  {rows.map((c) => (
                    <li key={c.category_id}>
                      <BudgetRow
                        row={c}
                        name={names.get(c.category_id) ?? "Category"}
                        currency={b.currency}
                        month={month}
                        open={editing === c.category_id}
                        onOpen={() => {
                          setEditing(editing === c.category_id ? null : c.category_id);
                          setMessage(null);
                        }}
                        onDone={(text) => {
                          setEditing(null);
                          setMessage(text);
                        }}
                      />
                    </li>
                  ))}
                </ul>
              </section>
            );
          })
        : null}

      <nav className="links">
        <Link to="/recurring">Recurring</Link>
        <Link to="/transactions">Transactions</Link>
        <Link to="/">Back to home</Link>
      </nav>
    </Page>
  );
}

/** "$40 spent", or "$500 returned" when refunds were larger than spending. */
function spentText(minor: number | null | undefined, currency: string): string {
  const value = minor ?? 0;
  return value < 0 ? `${money(-value, currency)} returned` : `${money(value, currency)} spent`;
}

/** One category: what was planned, spent, and left, with an inline plan editor. */
function BudgetRow({
  row: c,
  name,
  currency,
  month,
  open,
  onOpen,
  onDone,
}: {
  row: CategoryBudget;
  name: string;
  currency: string;
  month: string;
  open: boolean;
  onOpen: () => void;
  onDone: (message: string | null) => void;
}) {
  const yearly = c.kind === "non_monthly";
  let line: string;
  if (c.plan_minor === null || c.plan_minor === undefined) {
    line = c.spent_minor !== 0 ? `${spentText(c.spent_minor, currency)}. No plan.` : "No plan.";
  } else if (yearly) {
    line = `${money(c.plan_minor, currency)} a year. Set aside so far ${money(c.set_aside_minor, currency)}. This year: ${spentText(c.spent_this_year_minor, currency)}.`;
  } else {
    const left = c.left_minor ?? 0;
    const carried =
      c.carried_minor !== 0
        ? ` Includes ${money(c.carried_minor, currency)} from earlier months.`
        : "";
    line =
      left >= 0
        ? `${spentText(c.spent_minor, currency)} of ${money(c.plan_minor, currency)}. ${money(left, currency)} left.${carried}`
        : `${spentText(c.spent_minor, currency)} of ${money(c.plan_minor, currency)}. ${money(-left, currency)} more than planned.${carried}`;
  }
  return (
    <>
      <div className="budget-row">
        <span className="budget-text">
          <strong>{name}</strong>
          <span className="meta">{line}</span>
        </span>
        <Button variant="quiet" onPress={onOpen} aria-expanded={open}>
          {open ? "Close" : "Plan"}
        </Button>
      </div>
      {open ? (
        <PlanEditor row={c} name={name} currency={currency} month={month} onDone={onDone} />
      ) : null}
    </>
  );
}

function PlanEditor({
  row: c,
  name,
  currency,
  month,
  onDone,
}: {
  row: CategoryBudget;
  name: string;
  currency: string;
  month: string;
  onDone: (message: string | null) => void;
}) {
  const queryClient = useQueryClient();
  const field = useId();
  const yearly = c.kind === "non_monthly";
  const [amount, setAmount] = useState(
    c.plan_minor === null || c.plan_minor === undefined ? "" : money(c.plan_minor, currency),
  );
  const [rollover, setRollover] = useState(c.rollover);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function save(plan: number | null, text: string) {
    setBusy(true);
    setError(null);
    try {
      await setPlan(month, c.category_id, {
        plan_minor: plan,
        ...(yearly ? {} : { rollover }),
      });
      await queryClient.invalidateQueries({ queryKey: ["budget"] });
      onDone(text);
    } catch (e) {
      setError(describeError(e));
      setBusy(false);
    }
  }

  function submit(e: FormEvent) {
    e.preventDefault();
    const minor = parseAmount(amount, currency);
    if (minor === null || minor < 0) {
      setError("Enter an amount, for example 400 or 400.50.");
      return;
    }
    void save(minor, `Saved. The plan for ${name} applies from ${monthLabel(month)} on.`);
  }

  return (
    <form className="form budget-panel" onSubmit={submit} aria-label={`Plan for ${name}`}>
      <div className="field">
        <label htmlFor={`${field}-amount`}>{yearly ? "Yearly plan" : "Monthly plan"}</label>
        <input
          id={`${field}-amount`}
          inputMode="decimal"
          value={amount}
          onChange={(e) => setAmount(e.target.value)}
        />
      </div>
      {yearly ? null : (
        <label className="check">
          <input
            type="checkbox"
            checked={rollover}
            onChange={(e) => setRollover(e.target.checked)}
          />
          Carry what is left into next month
        </label>
      )}
      <div className="actions">
        <Button type="submit" isPending={busy}>
          Save plan
        </Button>
        {c.plan_minor !== null && c.plan_minor !== undefined ? (
          <Button
            variant="quiet"
            isPending={busy}
            onPress={() => save(null, `The plan for ${name} ends from ${monthLabel(month)}.`)}
          >
            Remove plan
          </Button>
        ) : null}
        <Button variant="quiet" onPress={() => onDone(null)}>
          Cancel
        </Button>
      </div>
      <Notice tone="problem">{error}</Notice>
    </form>
  );
}
