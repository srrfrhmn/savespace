import { useInfiniteQuery, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { type FormEvent, useId, useState } from "react";
import { accountsQuery } from "../connections/data";
import { bankDate, money, parseAmount } from "../format";
import { transactionInsightsQuery } from "../insights/data";
import { InsightCard } from "../insights/feed";
import {
  type Category,
  categoriesQuery,
  createMerchantRule,
  type Filters,
  noFilters,
  setSplits,
  type Transaction,
  type TransactionEdit,
  transactionsQuery,
  updateTransaction,
} from "../transactions/data";
import { Button, describeError, Notice, Page } from "../ui";

type Names = Map<string, string>;

function names(categories: Category[] | undefined): Names {
  return new Map((categories ?? []).map((c) => [c.id, c.name]));
}

/** "Groceries", "Split: Groceries and Home", or "No category". */
function categoryLine(t: Transaction, byId: Names): string {
  if (t.splits.length > 0) {
    const parts = [...new Set(t.splits.map((s) => byId.get(s.category_id) ?? "Other"))];
    return `Split: ${parts.join(" and ")}`;
  }
  return t.category_id ? (byId.get(t.category_id) ?? "Other") : "No category";
}

export function Transactions() {
  const [filters, setFilters] = useState<Filters>(noFilters);
  const [search, setSearch] = useState("");
  const [open, setOpen] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const categories = useQuery(categoriesQuery);
  const accounts = useQuery(accountsQuery);
  const list = useInfiniteQuery(transactionsQuery(filters));
  const byId = names(categories.data);
  const rows = list.data?.pages.flatMap((p) => p.data) ?? [];
  const filtered = JSON.stringify(filters) !== JSON.stringify(noFilters);

  function set<K extends keyof Filters>(key: K, value: Filters[K]) {
    setFilters((f) => ({ ...f, [key]: value }));
    setOpen(null);
  }

  function submit(e: FormEvent) {
    e.preventDefault();
    set("q", search);
  }

  return (
    <Page title="Transactions">
      <form className="filters" aria-label="Find transactions" onSubmit={submit}>
        <div className="field filters-search">
          <label htmlFor="q">Search</label>
          <div className="inline">
            <input
              id="q"
              type="search"
              maxLength={100}
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="Merchant, description, or note"
            />
            <Button type="submit" variant="quiet">
              Search
            </Button>
          </div>
        </div>
        <div className="field">
          <label htmlFor="account">Account</label>
          <select
            id="account"
            value={filters.account}
            onChange={(e) => set("account", e.target.value)}
          >
            <option value="">All accounts</option>
            {accounts.data?.map((a) => (
              <option key={a.id} value={a.id}>
                {a.mask ? `${a.name} (${a.mask})` : a.name}
              </option>
            ))}
          </select>
        </div>
        <div className="field">
          <label htmlFor="category">Category</label>
          <select
            id="category"
            value={filters.category}
            onChange={(e) => set("category", e.target.value)}
          >
            <option value="">All categories</option>
            <option value="none">No category</option>
            {categories.data?.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
        </div>
        <div className="field">
          <label htmlFor="from">From</label>
          <input
            id="from"
            type="date"
            value={filters.from}
            onChange={(e) => set("from", e.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor="to">To</label>
          <input
            id="to"
            type="date"
            value={filters.to}
            onChange={(e) => set("to", e.target.value)}
          />
        </div>
        <label className="check">
          <input
            type="checkbox"
            checked={filters.includeHidden}
            onChange={(e) => set("includeHidden", e.target.checked)}
          />
          Show hidden transactions
        </label>
        {filtered ? (
          <Button
            variant="quiet"
            onPress={() => {
              setFilters(noFilters);
              setSearch("");
            }}
          >
            Clear filters
          </Button>
        ) : null}
      </form>

      <Notice>{message}</Notice>
      {list.isError ? <Notice tone="problem">{describeError(list.error)}</Notice> : null}
      {list.isPending ? <p>Loading.</p> : null}
      {list.isSuccess && rows.length === 0 ? (
        <section className="card">
          <p>
            {filtered
              ? "No transactions match. Try other filters."
              : "No transactions yet. They show here after you connect an account."}
          </p>
        </section>
      ) : null}

      {rows.length > 0 ? (
        <ul className="txns" aria-label="Transactions">
          {rows.map((t) => (
            <li key={t.id}>
              <button
                type="button"
                className="txn-row"
                aria-expanded={open === t.id}
                aria-controls={`${t.id}-edit`}
                onClick={() => {
                  setOpen(open === t.id ? null : t.id);
                  setMessage(null);
                }}
              >
                <span>
                  <strong>{t.merchant_name ?? t.name}</strong>
                  <span className="meta">
                    {bankDate(t.date)}. {categoryLine(t, byId)}
                    {t.pending ? ". Pending" : ""}
                    {t.hidden ? ". Hidden" : ""}
                  </span>
                </span>
                <span className="amount">{money(t.amount_minor, t.currency)}</span>
              </button>
              {open === t.id ? (
                <EditPanel
                  id={`${t.id}-edit`}
                  transaction={t}
                  categories={categories.data ?? []}
                  onDone={(text) => {
                    setOpen(null);
                    setMessage(text);
                  }}
                />
              ) : null}
            </li>
          ))}
        </ul>
      ) : null}

      {list.hasNextPage ? (
        <Button
          variant="quiet"
          isPending={list.isFetchingNextPage}
          onPress={() => void list.fetchNextPage()}
        >
          Show more
        </Button>
      ) : null}

      <nav className="links">
        <Link to="/rules">Category rules</Link>
        <Link to="/accounts">Accounts</Link>
        <Link to="/">Back to home</Link>
      </nav>
    </Page>
  );
}

/** Edit one transaction: category, merchant name, note, hidden, and split. */
function EditPanel({
  id,
  transaction: t,
  categories,
  onDone,
}: {
  id: string;
  transaction: Transaction;
  categories: Category[];
  onDone: (message: string | null) => void;
}) {
  const queryClient = useQueryClient();
  const field = useId();
  const personCategory = t.category_source === "person" ? (t.category_id ?? "") : "";
  const [category, setCategory] = useState(personCategory);
  const [merchant, setMerchant] = useState(t.merchant_name ?? "");
  const [note, setNote] = useState(t.note ?? "");
  const [hidden, setHidden] = useState(t.hidden);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [suggest, setSuggest] = useState<{ merchantId: string; categoryId: string } | null>(null);
  const [splitting, setSplitting] = useState(false);
  const byId = names(categories);
  const automatic =
    t.category_source && t.category_source !== "person" && t.category_id
      ? `Automatic (${byId.get(t.category_id) ?? "Other"})`
      : "Automatic";

  async function save(e: FormEvent) {
    e.preventDefault();
    const edit: TransactionEdit = {};
    if (category !== personCategory) {
      edit.category_id = category || null;
    }
    if (merchant.trim() !== (t.merchant_name ?? "")) {
      edit.merchant_name = merchant.trim() || null;
    }
    if (note.trim() !== (t.note ?? "")) {
      edit.note = note.trim() || null;
    }
    if (hidden !== t.hidden) {
      edit.hidden = hidden;
    }
    if (Object.keys(edit).length === 0) {
      onDone(null);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await updateTransaction(t.id, edit);
      await queryClient.invalidateQueries({ queryKey: ["transactions"] });
      if (edit.category_id && t.merchant_id) {
        setSuggest({ merchantId: t.merchant_id, categoryId: edit.category_id });
      } else {
        onDone("Saved.");
      }
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  async function makeRule() {
    if (!suggest) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const rule = await createMerchantRule(suggest.merchantId, suggest.categoryId);
      await queryClient.invalidateQueries({ queryKey: ["transactions"] });
      await queryClient.invalidateQueries({ queryKey: ["rules"] });
      const name = byId.get(suggest.categoryId) ?? "that category";
      const count = rule.transactions;
      onDone(
        `Done. ${rule.merchant_name ?? "This merchant"} goes in ${name} from now on. ` +
          `${count} ${count === 1 ? "transaction" : "transactions"} changed.`,
      );
    } catch (err) {
      setError(describeError(err));
      setBusy(false);
    }
  }

  if (suggest) {
    const name = byId.get(suggest.categoryId) ?? "this category";
    return (
      <section id={id} className="panel" aria-label="Category rule">
        <p>
          Saved. Always put {t.merchant_name ?? "this merchant"} in {name}? This also changes past
          transactions. Your own edits stay.
        </p>
        <div className="actions">
          <Button isPending={busy} onPress={makeRule}>
            Yes, always
          </Button>
          <Button variant="quiet" onPress={() => onDone("Saved.")}>
            Just this one
          </Button>
        </div>
        <Notice tone="problem">{error}</Notice>
      </section>
    );
  }

  if (splitting) {
    return (
      <section id={id} className="panel" aria-label="Split transaction">
        <SplitEditor transaction={t} categories={categories} onDone={onDone} />
      </section>
    );
  }

  return (
    <section id={id} className="panel" aria-label="Edit transaction">
      <p className="meta">
        {t.name}. {money(t.amount_minor, t.currency)} on {bankDate(t.date)}.
      </p>
      <TransactionInsights id={t.id} />
      <form className="form" onSubmit={save}>
        <div className="field">
          <label htmlFor={`${field}-category`}>Category</label>
          <select
            id={`${field}-category`}
            value={category}
            onChange={(e) => setCategory(e.target.value)}
            disabled={t.splits.length > 0}
          >
            <option value="">{automatic}</option>
            {categories.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
          {t.splits.length > 0 ? (
            <span className="meta">This transaction is split. Change the parts below.</span>
          ) : null}
        </div>
        <div className="field">
          <label htmlFor={`${field}-merchant`}>Merchant name</label>
          <input
            id={`${field}-merchant`}
            maxLength={100}
            value={merchant}
            onChange={(e) => setMerchant(e.target.value)}
          />
        </div>
        <div className="field">
          <label htmlFor={`${field}-note`}>Note</label>
          <textarea
            id={`${field}-note`}
            maxLength={500}
            rows={2}
            value={note}
            onChange={(e) => setNote(e.target.value)}
          />
        </div>
        <label className="check">
          <input type="checkbox" checked={hidden} onChange={(e) => setHidden(e.target.checked)} />
          Hide from lists, budgets, and insights
        </label>
        <div className="actions">
          <Button type="submit" isPending={busy}>
            Save
          </Button>
          <Button variant="quiet" onPress={() => onDone(null)}>
            Cancel
          </Button>
          {t.pending ? null : (
            <Button variant="quiet" onPress={() => setSplitting(true)}>
              {t.splits.length > 0 ? "Change split" : "Split"}
            </Button>
          )}
        </div>
      </form>
      {t.pending ? <p className="meta">You can split this after it posts.</p> : null}
      <Notice tone="problem">{error}</Notice>
    </section>
  );
}

/** Insights about one transaction, for example "recurring and expected". */
function TransactionInsights({ id }: { id: string }) {
  const insights = useQuery(transactionInsightsQuery(id));
  if (!insights.data || insights.data.length === 0) {
    return null;
  }
  return (
    <ul className="insight-list">
      {insights.data.map((i) => (
        <InsightCard key={i.id} insight={i} />
      ))}
    </ul>
  );
}

type Part = { key: string; category: string; amount: string };

const newKey = () => crypto.randomUUID();

/** Divide a posted transaction across categories. Amounts are typed as positive numbers. */
function SplitEditor({
  transaction: t,
  categories,
  onDone,
}: {
  transaction: Transaction;
  categories: Category[];
  onDone: (message: string | null) => void;
}) {
  const queryClient = useQueryClient();
  const field = useId();
  const total = Math.abs(t.amount_minor);
  const sign = t.amount_minor < 0 ? -1 : 1;
  const start: Part[] =
    t.splits.length > 0
      ? t.splits.map((s) => ({
          key: s.id,
          category: s.category_id,
          amount: money(Math.abs(s.amount_minor), t.currency),
        }))
      : [
          { key: newKey(), category: t.category_id ?? "", amount: "" },
          { key: newKey(), category: "", amount: "" },
        ];
  const [parts, setParts] = useState<Part[]>(start);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const amounts = parts.map((p) => (p.amount.trim() ? parseAmount(p.amount, t.currency) : null));
  const assigned = amounts.reduce<number>((sum, a) => sum + (a ?? 0), 0);
  const left = total - assigned;
  const valid =
    parts.every((p, i) => p.category && amounts[i] !== null && (amounts[i] ?? 0) > 0) && left === 0;

  function change(index: number, part: Partial<Part>) {
    setParts((all) => all.map((p, i) => (i === index ? { ...p, ...part } : p)));
  }

  async function save(splits: { category_id: string; amount_minor: number }[], done: string) {
    setBusy(true);
    setError(null);
    try {
      await setSplits(t.id, splits);
      await queryClient.invalidateQueries({ queryKey: ["transactions"] });
      onDone(done);
    } catch (err) {
      setError(describeError(err));
      setBusy(false);
    }
  }

  function submit(e: FormEvent) {
    e.preventDefault();
    if (!valid) {
      setError("Give each part a category and an amount. The parts must add up to the total.");
      return;
    }
    void save(
      parts.map((p, i) => ({ category_id: p.category, amount_minor: sign * (amounts[i] ?? 0) })),
      "Split saved.",
    );
  }

  return (
    <form className="form split" onSubmit={submit}>
      <p>
        Split {money(total, t.currency)} from {t.merchant_name ?? t.name}.
      </p>
      {parts.map((p, i) => (
        <fieldset key={p.key} className="split-part">
          <legend>Part {i + 1}</legend>
          <div className="field">
            <label htmlFor={`${field}-${i}-category`}>Category</label>
            <select
              id={`${field}-${i}-category`}
              value={p.category}
              onChange={(e) => change(i, { category: e.target.value })}
            >
              <option value="">Choose a category</option>
              {categories.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                </option>
              ))}
            </select>
          </div>
          <div className="field">
            <label htmlFor={`${field}-${i}-amount`}>Amount</label>
            <input
              id={`${field}-${i}-amount`}
              inputMode="decimal"
              value={p.amount}
              aria-invalid={p.amount.trim() !== "" && amounts[i] === null}
              onChange={(e) => change(i, { amount: e.target.value })}
            />
          </div>
          {parts.length > 2 ? (
            <Button
              variant="quiet"
              onPress={() => setParts((all) => all.filter((_, j) => j !== i))}
            >
              Remove part {i + 1}
            </Button>
          ) : null}
        </fieldset>
      ))}
      <p role="status">
        {left === 0
          ? "All of it is assigned."
          : left > 0
            ? `${money(left, t.currency)} left to assign.`
            : `${money(-left, t.currency)} more than the total.`}
      </p>
      <div className="actions">
        {parts.length < 10 ? (
          <Button
            variant="quiet"
            onPress={() => setParts((all) => [...all, { key: newKey(), category: "", amount: "" }])}
          >
            Add a part
          </Button>
        ) : null}
        <Button type="submit" isPending={busy}>
          Save split
        </Button>
        {t.splits.length > 0 ? (
          <Button variant="quiet" isPending={busy} onPress={() => save([], "Split removed.")}>
            Remove split
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
