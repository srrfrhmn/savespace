import { describe, expect, it } from "vitest";
import { percent, type Review, type ReviewItem, withVerdict } from "./data";

function item(id: string, verdict: string | null, status = "open"): ReviewItem {
  return {
    id,
    rule_id: "budget.month_on_track",
    kind: "reassurance",
    text: "t",
    facts: [],
    status,
    created_at: "2026-11-16T12:00:00Z",
    verdict,
    verdict_source: verdict ? "review" : null,
  };
}

function review(items: ReviewItem[]): Review {
  return {
    data: items,
    summary: { total: items.length, correct: 0, wrong: 0, rate_bp: null, target_bp: 9950 },
  };
}

describe("withVerdict", () => {
  it("recounts and rounds down", () => {
    const r = withVerdict(
      review([item("a", "correct"), item("b", "correct"), item("c", null)]),
      "c",
      "wrong",
    );
    expect(r.summary).toMatchObject({ correct: 2, wrong: 1, rate_bp: 6666 });
  });

  it("falls back to the app feedback when a review is cleared", () => {
    const r = withVerdict(review([item("a", "correct", "wrong")]), "a", null);
    expect(r.data[0]).toMatchObject({ verdict: "wrong", verdict_source: "feedback" });
    expect(r.summary.rate_bp).toBe(0);
    const open = withVerdict(review([item("b", "correct")]), "b", null);
    expect(open.data[0]?.verdict).toBeNull();
    expect(open.summary.rate_bp).toBeNull();
  });
});

describe("percent", () => {
  it("shows two decimals", () => {
    expect(percent(9950)).toBe("99.50%");
    expect(percent(10000)).toBe("100.00%");
    expect(percent(6666)).toBe("66.66%");
  });
});
