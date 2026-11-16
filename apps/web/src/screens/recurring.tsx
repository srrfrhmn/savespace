import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { type RecurringSeries, recurringQuery } from "../budget/data";
import { bankDate, money } from "../format";
import { describeError, Notice, Page } from "../ui";

const CADENCE: Record<string, string> = {
  weekly: "week",
  biweekly: "2 weeks",
  monthly: "month",
  annual: "year",
};

/** "About $15.49 each month." Amounts are shown as positive numbers. */
export function seriesLine(s: RecurringSeries): string {
  const each = CADENCE[s.cadence] ?? s.cadence;
  const amount = money(Math.abs(s.amount_minor), s.currency);
  const about = s.spread_minor > 0 ? "About " : "";
  return `${about}${amount} each ${each}.`;
}

/** Active series due in the next `days` days, soonest first. */
export function upcoming(list: RecurringSeries[], days: number): RecurringSeries[] {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const end = today.getTime() + days * 86_400_000;
  return list
    .filter((s) => s.active)
    .filter((s) => {
      const t = new Date(`${s.next_date}T00:00:00`).getTime();
      return t >= today.getTime() && t <= end;
    })
    .sort((a, b) => a.next_date.localeCompare(b.next_date));
}

function Row({ s }: { s: RecurringSeries }) {
  const kind = s.direction === "in" ? "Money in" : "Charge";
  let status: string;
  if (!s.active) {
    status = `Seems to have stopped. Last on ${bankDate(s.last_date)}.`;
  } else if (s.expected) {
    status = `Expected next on ${bankDate(s.next_date)}.`;
  } else {
    status = `Maybe next on ${bankDate(s.next_date)}. The pattern is not clear yet.`;
  }
  return (
    <li>
      <span>
        <strong>{s.merchant_name}</strong>
        <span className="meta">
          {kind}. {seriesLine(s)} {status}
        </span>
      </span>
    </li>
  );
}

export function Recurring() {
  const list = useQuery(recurringQuery);
  const data = list.data ?? [];
  const soon = upcoming(data, 30);

  return (
    <Page title="Recurring">
      <p className="lead">
        Charges and income that repeat. SaveSpace finds them from your transactions, so you know
        what to expect.
      </p>
      {list.isError ? <Notice tone="problem">{describeError(list.error)}</Notice> : null}
      {list.isPending ? <p>Loading.</p> : null}
      {list.isSuccess && data.length === 0 ? (
        <section className="card">
          <p>
            Nothing recurring found yet. SaveSpace needs a few months of transactions to see a
            pattern.
          </p>
        </section>
      ) : null}

      {soon.length > 0 ? (
        <section className="card" aria-labelledby="soon-h">
          <h2 id="soon-h">In the next 30 days</h2>
          <ul className="list">
            {soon.map((s) => (
              <Row key={s.id} s={s} />
            ))}
          </ul>
        </section>
      ) : null}

      {data.length > 0 ? (
        <section className="card" aria-labelledby="all-h">
          <h2 id="all-h">All recurring</h2>
          <ul className="list">
            {data.map((s) => (
              <Row key={s.id} s={s} />
            ))}
          </ul>
        </section>
      ) : null}

      <nav className="links">
        <Link to="/budget">Budget</Link>
        <Link to="/">Back to home</Link>
      </nav>
    </Page>
  );
}
