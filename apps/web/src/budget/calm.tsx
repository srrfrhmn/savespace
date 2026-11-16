import { useState } from "react";
import { bankDate, money } from "../format";
import { Button } from "../ui";
import type { Calm } from "./data";

/**
 * The calm state (design doc §6.1). In calm mode the amounts stay hidden until
 * the person asks. "Needs attention" always shows them: information that needs
 * action is never hidden (§5, Gentle exposure).
 */
export function CalmCard({
  calm,
  currency,
  calmMode,
}: {
  calm: Calm;
  currency: string;
  calmMode: boolean;
}) {
  const [revealed, setRevealed] = useState(false);
  const until = bankDate(calm.until);
  const text =
    {
      covered: `Covered. Your cash covers the bills expected through ${until} and the rest of your flexible plan.`,
      tight: `Tight. Your cash covers the bills expected through ${until}, but not all of your flexible plan.`,
      needs_attention: `Needs attention. Your cash may not cover the bills expected through ${until}.`,
    }[calm.state] ?? "";
  const show = !calmMode || revealed || calm.state === "needs_attention";

  return (
    <section className={`card calm calm-${calm.state}`} aria-label="Money status">
      <p className="calm-state">{text}</p>
      {show ? (
        <ul className="facts">
          <li>
            Cash in checking and savings: <strong>{money(calm.cash_minor, currency)}</strong>
          </li>
          <li>
            Bills expected through {until}: <strong>{money(calm.bills_minor, currency)}</strong>
          </li>
          <li>
            Left in your flexible plan: <strong>{money(calm.flex_left_minor, currency)}</strong>
          </li>
        </ul>
      ) : null}
      {calmMode && calm.state !== "needs_attention" ? (
        <Button variant="quiet" onPress={() => setRevealed(!revealed)}>
          {revealed ? "Hide amounts" : "Show amounts"}
        </Button>
      ) : null}
    </section>
  );
}
