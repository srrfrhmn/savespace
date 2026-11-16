import { useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useId, useState } from "react";
import { Button, describeError, Notice } from "../ui";
import { type Insight, sendFeedback } from "./data";

const REASONS = [
  { value: "amount", label: "The amount" },
  { value: "merchant", label: "The merchant" },
  { value: "category", label: "The category" },
  { value: "date", label: "The date" },
  { value: "other", label: "Something else" },
] as const;

/**
 * One insight with its controls (design doc §7.7): "Why am I seeing this?",
 * "This is wrong", and "Not helpful".
 */
export function InsightCard({ insight }: { insight: Insight }) {
  const queryClient = useQueryClient();
  const id = useId();
  const [why, setWhy] = useState(false);
  const [wrong, setWrong] = useState(false);
  const [reason, setReason] = useState<string>("amount");
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function send(kind: "wrong" | "not_helpful", r?: string) {
    setBusy(true);
    setError(null);
    try {
      const muted = await sendFeedback(insight.id, kind, r);
      setDone(
        kind === "wrong"
          ? "Thank you. This insight is hidden, and SaveSpace will check it."
          : muted
            ? "Thank you. You will not see this kind of insight again. You can turn it back on in Security."
            : "Thank you. You will see fewer insights like this.",
      );
      await queryClient.invalidateQueries({ queryKey: ["insights"] });
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }

  function submitWrong(e: FormEvent) {
    e.preventDefault();
    void send("wrong", reason);
  }

  if (done) {
    return (
      <li className="insight-card">
        <p role="status">{done}</p>
      </li>
    );
  }
  return (
    <li className={`insight-card insight-${insight.kind}`}>
      <p className="insight-text">{insight.text}</p>
      <div className="actions insight-actions">
        <Button
          variant="quiet"
          aria-expanded={why}
          aria-controls={`${id}-why`}
          onPress={() => setWhy(!why)}
        >
          Why am I seeing this?
        </Button>
        <Button variant="quiet" aria-expanded={wrong} onPress={() => setWrong(!wrong)}>
          This is wrong
        </Button>
        <Button variant="quiet" isPending={busy && !wrong} onPress={() => send("not_helpful")}>
          Not helpful
        </Button>
      </div>
      {why ? (
        <dl id={`${id}-why`} className="facts-list">
          {insight.facts.map((f) => (
            <div key={f.key}>
              <dt>{f.label}</dt>
              <dd>{f.value}</dd>
            </div>
          ))}
        </dl>
      ) : null}
      {wrong ? (
        <form className="form" onSubmit={submitWrong}>
          <fieldset className="reasons">
            <legend>What is wrong?</legend>
            {REASONS.map((r) => (
              <label key={r.value} className="check">
                <input
                  type="radio"
                  name={`${id}-reason`}
                  value={r.value}
                  checked={reason === r.value}
                  onChange={() => setReason(r.value)}
                />
                {r.label}
              </label>
            ))}
          </fieldset>
          <Button type="submit" isPending={busy}>
            Send
          </Button>
        </form>
      ) : null}
      <Notice tone="problem">{error}</Notice>
    </li>
  );
}

export function InsightFeed({ insights }: { insights: Insight[] }) {
  if (insights.length === 0) {
    return null;
  }
  return (
    <section aria-label="Insights">
      <ul className="insight-list">
        {insights.map((i) => (
          <InsightCard key={i.id} insight={i} />
        ))}
      </ul>
    </section>
  );
}
