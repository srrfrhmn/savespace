# Insight LLM evaluations

Phase 1 adds the eval set. See design doc §7.5.

## Planned format

One JSON Lines file per insight rule. Each line is one case:

```json
{
  "id": "spend.week_below_usual/0001",
  "rule_id": "spend.week_below_usual",
  "rule_version": 1,
  "kind": "reassurance",
  "locale": "en-US",
  "facts": { "wtd_spend": "$212", "baseline_median": "$268" },
  "template_text": "You spent less than usual this week: $212 so far, compared to a usual $268.",
  "expect": { "numbers": ["$212", "$268"], "max_chars": 140 }
}
```

## Release gate

CI runs the set when the prompt, the model, or a template changes:

- The validator pass rate is ≥ 98%.
- A second, independent parser finds zero number-integrity failures in texts that passed the validator.
- The human rubric score (calm, correct, clear) is ≥ 4.5 / 5 on a 50-item sample.
