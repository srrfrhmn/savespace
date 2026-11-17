import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useId, useState } from "react";
import {
  percent,
  type Review,
  type ReviewItem,
  reviewQuery,
  setReview,
  type Verdict,
  withVerdict,
} from "../insights/data";
import { describeError, Notice, Page } from "../ui";

const WINDOWS = [7, 28, 90] as const;

const STATUS: Record<string, string> = {
  open: "Showing",
  expired: "No longer true",
  dismissed: "Not helpful",
  wrong: "Marked wrong in the app",
};

function when(value: string): string {
  return new Date(value).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

function ReviewCard({
  item,
  onVerdict,
}: {
  item: ReviewItem;
  onVerdict: (verdict: Verdict | null) => void;
}) {
  const id = useId();
  const value = item.verdict ?? "";
  return (
    <li className="card review-card">
      <p className="insight-text">{item.text}</p>
      <p className="meta">
        {when(item.created_at)} · {item.rule_id} · {STATUS[item.status] ?? item.status}
      </p>
      <dl className="facts-list">
        {item.facts.map((f) => (
          <div key={f.key}>
            <dt>{f.label}</dt>
            <dd>{f.value}</dd>
          </div>
        ))}
      </dl>
      <fieldset className="verdict">
        <legend>Is this true?</legend>
        {(
          [
            ["correct", "Correct"],
            ["wrong", "Wrong"],
            ["", "Not reviewed"],
          ] as const
        ).map(([v, label]) => (
          <label key={v} className="check">
            <input
              type="radio"
              name={`${id}-verdict`}
              value={v}
              checked={value === v}
              onChange={() => onVerdict(v === "" ? null : v)}
            />
            {label}
          </label>
        ))}
      </fieldset>
      {item.verdict_source === "feedback" ? (
        <p className="meta">Counted as wrong because of "This is wrong" in the app.</p>
      ) : null}
    </li>
  );
}

/**
 * The owner's correctness review (design doc §16): mark each insight correct
 * or wrong. Phase 1 needs at least 99.5% over 4 weeks.
 */
export function ReviewScreen() {
  const queryClient = useQueryClient();
  const [days, setDays] = useState<number>(28);
  const [onlyOpen, setOnlyOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const query = reviewQuery(days);
  const review = useQuery(query);
  const r = review.data;

  async function decide(item: ReviewItem, verdict: Verdict | null) {
    setError(null);
    const before = queryClient.getQueryData<Review>(query.queryKey);
    queryClient.setQueryData<Review>(query.queryKey, (old) =>
      old ? withVerdict(old, item.id, verdict) : old,
    );
    try {
      await setReview(item.id, verdict);
    } catch (e) {
      queryClient.setQueryData(query.queryKey, before);
      setError(describeError(e));
    }
  }

  const items = r ? r.data.filter((i) => !onlyOpen || i.verdict === null) : [];
  const s = r?.summary;
  const reviewed = s ? s.correct + s.wrong : 0;
  const meets = s?.rate_bp != null && s.rate_bp >= s.target_bp;

  return (
    <Page title="Insight review">
      <p>
        Check each insight against your accounts. Phase 1 is done when at least{" "}
        {s ? percent(s.target_bp) : "99.50%"} of reviewed insights over 4 weeks are correct.
      </p>
      <div className="inline">
        <div className="field">
          <label htmlFor="window">Insights from the last</label>
          <select id="window" value={days} onChange={(e) => setDays(Number(e.target.value))}>
            {WINDOWS.map((d) => (
              <option key={d} value={d}>
                {d} days
              </option>
            ))}
          </select>
        </div>
        <label className="check">
          <input
            type="checkbox"
            checked={onlyOpen}
            onChange={(e) => setOnlyOpen(e.target.checked)}
          />
          Only not reviewed
        </label>
      </div>
      <Notice tone="problem">{error}</Notice>
      {review.isError ? <Notice tone="problem">{describeError(review.error)}</Notice> : null}
      {review.isPending ? <p>Loading.</p> : null}
      {s ? (
        <section className="card" aria-labelledby="rate-h">
          <h2 id="rate-h">Correctness</h2>
          <p className="lead" aria-live="polite">
            {s.rate_bp == null
              ? "No insights reviewed yet."
              : `${percent(s.rate_bp)} correct: ${s.correct} correct, ${s.wrong} wrong.`}
          </p>
          <p className="meta">
            {reviewed} of {s.total} reviewed.{" "}
            {s.rate_bp == null ? "" : meets ? "Meets the target." : "Below the target."}
          </p>
        </section>
      ) : null}
      {r && items.length === 0 ? (
        <p>{r.data.length === 0 ? "No insights in this window." : "Every insight is reviewed."}</p>
      ) : null}
      <ul className="review-list">
        {items.map((i) => (
          <ReviewCard key={i.id} item={i} onVerdict={(v) => void decide(i, v)} />
        ))}
      </ul>
      <nav className="links">
        <Link to="/settings">Back to settings</Link>
      </nav>
    </Page>
  );
}
