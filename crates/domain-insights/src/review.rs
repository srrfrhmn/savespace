//! The owner's correctness review (design doc §16, Phase 1 exit criterion:
//! insight correctness of at least 99.5% over 4 weeks).
//!
//! Every insight made in the window counts, also those the view policy did
//! not show. A verdict comes from a review, or else from "This is wrong"
//! feedback. A review overrides the feedback.

use serde_json::Value;
use uuid::Uuid;

use crate::model::Kind;
use crate::service::{InsightError, InsightService, Result};

/// The Phase 1 target, in basis points.
pub const TARGET_BP: u32 = 9950;

/// A verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The text and facts were true.
    Correct,
    /// Something in it was not true.
    Wrong,
}

impl Verdict {
    /// The stable name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Correct => "correct",
            Self::Wrong => "wrong",
        }
    }

    /// Parses [`Verdict::as_str`].
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Correct, Self::Wrong]
            .into_iter()
            .find(|v| v.as_str() == value)
    }
}

/// Where a verdict came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The review page.
    Review,
    /// "This is wrong" in the app.
    Feedback,
}

/// One insight in the review.
#[derive(Debug, Clone)]
pub struct ReviewItem {
    /// The insight ID.
    pub id: Uuid,
    /// The rule.
    pub rule_id: String,
    /// The kind.
    pub kind: Kind,
    /// The text.
    pub text: String,
    /// The facts.
    pub facts: Value,
    /// `open`, `expired`, `dismissed`, or `wrong`.
    pub status: String,
    /// When it was made.
    pub created_at: jiff::Timestamp,
    /// The verdict, if any.
    pub verdict: Option<(Verdict, Source)>,
}

/// The counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Insights in the window.
    pub total: u32,
    /// Correct.
    pub correct: u32,
    /// Wrong.
    pub wrong: u32,
    /// Correct out of correct plus wrong, in basis points. `None` before the
    /// first verdict.
    pub rate_bp: Option<u32>,
}

impl Summary {
    /// Counts the verdicts.
    pub fn of(items: &[ReviewItem]) -> Self {
        let mut s = Self {
            total: u32::try_from(items.len()).unwrap_or(u32::MAX),
            ..Self::default()
        };
        for item in items {
            match item.verdict {
                Some((Verdict::Correct, _)) => s.correct += 1,
                Some((Verdict::Wrong, _)) => s.wrong += 1,
                None => {}
            }
        }
        let reviewed = u64::from(s.correct) + u64::from(s.wrong);
        // Round down, so 99.49% never shows as meeting 99.5%.
        s.rate_bp = (u64::from(s.correct) * 10_000)
            .checked_div(reviewed)
            .and_then(|r| u32::try_from(r).ok());
        s
    }
}

type ReviewRow = (
    Uuid,
    String,
    String,
    String,
    Value,
    String,
    jiff_sqlx::Timestamp,
    Option<String>,
    bool,
);

impl InsightService {
    /// The insights made since `since`, newest first, with their verdicts.
    ///
    /// # Errors
    ///
    /// A database error.
    pub async fn review(
        &self,
        household_id: Uuid,
        since: jiff::Timestamp,
    ) -> Result<Vec<ReviewItem>> {
        let mut tx = platform_db::tenant_tx(self.pool(), household_id).await?;
        let rows: Vec<ReviewRow> = sqlx::query_as(
            "SELECT i.id, i.rule_id, i.kind, i.text, i.facts, i.status, i.created_at, \
               r.verdict, EXISTS (SELECT 1 FROM app.insight_feedback f \
                                  WHERE f.insight_id = i.id AND f.kind = 'wrong') \
             FROM app.insights i LEFT JOIN app.insight_reviews r ON r.insight_id = i.id \
             WHERE i.created_at >= $1 ORDER BY i.created_at DESC, i.id DESC",
        )
        .bind(jiff_sqlx::Timestamp::from(since))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let verdict = match (r.7.as_deref().and_then(Verdict::parse), r.8) {
                    (Some(v), _) => Some((v, Source::Review)),
                    (None, true) => Some((Verdict::Wrong, Source::Feedback)),
                    (None, false) => None,
                };
                Some(ReviewItem {
                    id: r.0,
                    rule_id: r.1,
                    kind: Kind::parse(&r.2)?,
                    text: r.3,
                    facts: r.4,
                    status: r.5,
                    created_at: r.6.to_jiff(),
                    verdict,
                })
            })
            .collect())
    }

    /// Sets or clears the review verdict for one insight.
    ///
    /// # Errors
    ///
    /// [`InsightError::NotFound`] or a database error.
    pub async fn set_review(
        &self,
        household_id: Uuid,
        insight_id: Uuid,
        verdict: Option<Verdict>,
    ) -> Result<()> {
        let mut tx = platform_db::tenant_tx(self.pool(), household_id).await?;
        let found: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM app.insights WHERE id = $1)")
                .bind(insight_id)
                .fetch_one(&mut *tx)
                .await?;
        if !found {
            return Err(InsightError::NotFound);
        }
        match verdict {
            Some(v) => {
                sqlx::query(
                    "INSERT INTO app.insight_reviews (insight_id, household_id, verdict) \
                     VALUES ($1, $2, $3) ON CONFLICT (insight_id) \
                     DO UPDATE SET verdict = EXCLUDED.verdict, reviewed_at = now()",
                )
                .bind(insight_id)
                .bind(household_id)
                .bind(v.as_str())
                .execute(&mut *tx)
                .await?;
            }
            None => {
                sqlx::query("DELETE FROM app.insight_reviews WHERE insight_id = $1")
                    .bind(insight_id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(verdict: Option<Verdict>) -> ReviewItem {
        ReviewItem {
            id: Uuid::nil(),
            rule_id: "r".into(),
            kind: Kind::Reassurance,
            text: String::new(),
            facts: Value::Null,
            status: "open".into(),
            created_at: jiff::Timestamp::UNIX_EPOCH,
            verdict: verdict.map(|v| (v, Source::Review)),
        }
    }

    #[test]
    fn summary_rounds_down() {
        let mut items: Vec<_> = (0..199).map(|_| item(Some(Verdict::Correct))).collect();
        items.push(item(Some(Verdict::Wrong)));
        items.push(item(None));
        let s = Summary::of(&items);
        assert_eq!((s.total, s.correct, s.wrong), (201, 199, 1));
        assert_eq!(s.rate_bp, Some(9950));

        items.push(item(Some(Verdict::Wrong)));
        // 199 / 201 = 99.004...%
        assert_eq!(Summary::of(&items).rate_bp, Some(9900));
    }

    #[test]
    fn no_verdicts_no_rate() {
        assert_eq!(Summary::of(&[item(None)]).rate_bp, None);
        assert_eq!(Summary::of(&[]).total, 0);
    }

    #[test]
    fn verdict_names() {
        for v in [Verdict::Correct, Verdict::Wrong] {
            assert_eq!(Verdict::parse(v.as_str()), Some(v));
        }
        assert_eq!(Verdict::parse("maybe"), None);
    }
}
