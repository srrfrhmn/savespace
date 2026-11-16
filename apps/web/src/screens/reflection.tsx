import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { bankDate } from "../format";
import { isoDate, reflectionQuery } from "../insights/data";
import { Button, describeError, Notice, Page } from "../ui";

function shiftWeek(week: string, by: number): string {
  const [y, m, d] = week.split("-").map(Number);
  return isoDate(new Date(y ?? 2000, (m ?? 1) - 1, (d ?? 1) + 7 * by));
}

/** The weekly reflection (design doc §6.1): at most 5 cards. */
export function ReflectionScreen() {
  const today = isoDate(new Date());
  const [week, setWeek] = useState(today);
  const reflection = useQuery(reflectionQuery(week));
  const r = reflection.data;
  const isCurrent = r?.partial ?? week === today;

  return (
    <Page title="Weekly reflection">
      <nav className="month-nav" aria-label="Week">
        <Button variant="quiet" onPress={() => setWeek(shiftWeek(week, -1))}>
          Previous week
        </Button>
        <h2 aria-live="polite">{r ? `Week of ${bankDate(r.week_start)}` : "Week"}</h2>
        <Button variant="quiet" isDisabled={isCurrent} onPress={() => setWeek(shiftWeek(week, 1))}>
          Next week
        </Button>
      </nav>
      {reflection.isError ? (
        <Notice tone="problem">{describeError(reflection.error)}</Notice>
      ) : null}
      {reflection.isPending ? <p>Loading.</p> : null}
      {r?.partial ? <p className="meta">This week is not over yet.</p> : null}
      {r ? (
        <ol className="reflection">
          {r.cards.map((c) => (
            <li key={c.text} className="card">
              <p>{c.text}</p>
            </li>
          ))}
        </ol>
      ) : null}
      <nav className="links">
        <Link to="/">Back to home</Link>
      </nav>
    </Page>
  );
}
