import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useId, useState } from "react";
import { Button, describeError, Notice } from "../ui";
import { checkInsQuery, isoDate, saveCheckIn } from "./data";

const SCALE = [
  { value: 1, label: "1: Very uneasy" },
  { value: 2, label: "2: Uneasy" },
  { value: 3, label: "3: Okay" },
  { value: 4, label: "4: At ease" },
  { value: 5, label: "5: Very at ease" },
] as const;

/** The money check-in (design doc §6.1). One question, 1 to 5, an optional note. */
export function CheckInCard() {
  const queryClient = useQueryClient();
  const id = useId();
  const list = useQuery(checkInsQuery);
  const [score, setScore] = useState<number | null>(null);
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const today = isoDate(new Date());
  const doneToday = list.data?.some((c) => c.date === today) ?? false;
  if (list.isPending || (doneToday && !saved)) {
    return null;
  }
  if (saved) {
    return (
      <section className="card" aria-label="Check-in">
        <p role="status">Saved. Thank you for checking in.</p>
      </section>
    );
  }

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (score === null) {
      setError("Choose a number from 1 to 5.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await saveCheckIn(score, note);
      setSaved(true);
      await queryClient.invalidateQueries({ queryKey: checkInsQuery.queryKey });
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="card" aria-labelledby={`${id}-h`}>
      <form className="form checkin" onSubmit={submit}>
        <fieldset className="scale">
          <legend id={`${id}-h`}>How do you feel about money today?</legend>
          {SCALE.map((s) => (
            <label key={s.value} className="check">
              <input
                type="radio"
                name={`${id}-score`}
                value={s.value}
                checked={score === s.value}
                onChange={() => setScore(s.value)}
              />
              {s.label}
            </label>
          ))}
        </fieldset>
        <div className="field">
          <label htmlFor={`${id}-note`}>Note (optional, only you can read it)</label>
          <textarea
            id={`${id}-note`}
            rows={2}
            maxLength={500}
            value={note}
            onChange={(e) => setNote(e.target.value)}
          />
        </div>
        <Button type="submit" isPending={busy}>
          Save check-in
        </Button>
        <Notice tone="problem">{error}</Notice>
      </form>
    </section>
  );
}
