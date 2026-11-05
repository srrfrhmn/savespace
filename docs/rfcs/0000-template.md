# RFC NNNN: Title

| Field | Value |
| --- | --- |
| Status | Draft / In review / Accepted / Rejected / Superseded |
| Author | |
| Reviewers | |
| Date | YYYY-MM-DD |

Write an RFC for a change that adds a table, an external dependency, an insight rule, or a new data flow (design doc §14.4). Follow the writing rules in design doc Appendix A. Copy this file to `NNNN-short-name.md`.

## 1. Problem

What problem does this change solve? Who has the problem?

## 2. Proposal

What do we build? Include the API, data flow, and main types.

## 3. Alternatives

What other options exist? Why do we not choose them?

## 4. Data model

New or changed tables and columns. Migration plan (expand and contract, §9.3).

## 5. Security (STRIDE)

| Threat | Risk | Mitigation |
| --- | --- | --- |
| Spoofing | | |
| Tampering | | |
| Repudiation | | |
| Information disclosure | | |
| Denial of service | | |
| Elevation of privilege | | |

## 6. Privacy

What personal data does this change read, store, or send? Where? For how long? Who can access it?

## 7. Behavioral review

Required for insights, notifications, and user-visible copy (design doc §5).

- Which technique in §5.1 does this change use? Which guardrail applies?
- Does the change use any banned pattern in §5.2?
- Can the change cause false reassurance?

## 8. Rollout

Feature flag, phases, kill switch, rollback plan.

## 9. Metrics

How do we know the change works? Which guardrail metrics (§15.3) can it affect?

## 10. Open questions
