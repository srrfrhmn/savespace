## Summary

<!-- What does this change do, and why? Link the issue or RFC. -->

## Test plan

<!-- How did you verify the change? Commands, screenshots, test names. -->

## Checklist

- [ ] Tests cover the change. Money math has property tests.
- [ ] No new floating-point math for money. Amounts use `Money` (minor units + currency).
- [ ] Migrations follow expand and contract (design doc §9.3). No long locks.
- [ ] No secrets, tokens, or PII in logs (design doc §12.4).
- [ ] API changes: OpenAPI diff reviewed. Breaking changes use a new API version (§11.2).

### Behavioral review (design doc §5)

Complete this section if the change touches insights, notifications, or user-visible copy.

- [ ] No banned pattern (§5.2): no streaks with loss, variable rewards, red badges for non-urgent items, countdowns, population comparison, confirmshaming, or re-engagement-only notifications.
- [ ] Copy follows the style guide (Appendix B). No banned words.
- [ ] A reassurance insight cannot appear next to an open concern for the same scope (§7.2).
- [ ] Insight rule or copy change: reviewed by the insights owner and the financial therapy reviewer.
