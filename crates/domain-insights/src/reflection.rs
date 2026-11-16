//! The weekly reflection (design doc §6.1, §5 Peak-end rule).
//!
//! A fixed-length summary: at most 5 cards. It ends on a positive or neutral
//! true fact. If there is no positive fact, it ends on a neutral one. It never
//! invents a fact.

use savespace_core::{Currency, Day};
use serde::Serialize;

use crate::model::{money, short_date};

/// The tone of a card. The last card is never `Higher`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// A good, true fact.
    Positive,
    /// A plain fact.
    Neutral,
    /// Higher than usual. Never the last card.
    Higher,
}

/// One card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Card {
    /// The tone.
    pub tone: Tone,
    /// The text.
    pub text: String,
}

/// The facts for one week (Monday to Sunday).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeekSummary {
    /// The Monday.
    pub week_start: Day,
    /// True while the week is not over.
    pub partial: bool,
    /// The currency.
    pub currency: Currency,
    /// Flexible spending (positive).
    pub flex_spent: i64,
    /// Flexible spending in earlier complete weeks, newest first.
    pub history: Vec<i64>,
    /// The largest flexible category and its spending.
    pub top_category: Option<(String, i64)>,
    /// Expected recurring charges that posted: count and total (positive).
    pub recurring: (u32, i64),
    /// Money in, in income categories.
    pub income: i64,
    /// Days this week with no flexible spending (up to today if partial).
    pub no_spend_days: u32,
    /// Transactions this week.
    pub transactions: u32,
}

/// The cards for one week, in order. At most 5.
pub fn reflection(w: &WeekSummary) -> Vec<Card> {
    let m = |minor| money(minor, w.currency);
    let mut middle = Vec::new();
    let mut endings = Vec::new();
    let span = if w.partial {
        "this week so far".to_owned()
    } else {
        format!("in the week of {}", short_date(w.week_start))
    };

    middle.push(Card {
        tone: Tone::Neutral,
        text: format!("You spent {} on flexible spending {span}.", m(w.flex_spent)),
    });
    if w.history.len() >= crate::rules::MIN_WEEKS
        && !w.partial
        && let Some(usual) = savespace_core::stats::median(&w.history)
    {
        {
            let threshold = (usual / 20).max(1000);
            if w.flex_spent <= usual - threshold {
                endings.push(Card {
                    tone: Tone::Positive,
                    text: format!("That is less than usual. A usual week is {}.", m(usual)),
                });
            } else if w.flex_spent >= usual + threshold {
                middle.push(Card {
                    tone: Tone::Higher,
                    text: format!("That is higher than usual. A usual week is {}.", m(usual)),
                });
            } else {
                middle.push(Card {
                    tone: Tone::Neutral,
                    text: format!("That is close to a usual week of {}.", m(usual)),
                });
            }
        }
    }
    if w.history.len() < crate::rules::MIN_WEEKS {
        // §7.3: no comparison without enough history.
        middle.push(Card {
            tone: Tone::Neutral,
            text: "SaveSpace needs a few more weeks to learn what is usual for you.".to_owned(),
        });
    }
    if let Some((name, amount)) = &w.top_category {
        middle.push(Card {
            tone: Tone::Neutral,
            text: format!("Your largest flexible category was {name}: {}.", m(*amount)),
        });
    }
    let (count, total) = w.recurring;
    if count > 0 {
        let charges = if count == 1 { "charge" } else { "charges" };
        middle.push(Card {
            tone: Tone::Neutral,
            text: format!(
                "{count} expected recurring {charges} came through, for {}.",
                m(total)
            ),
        });
    }
    if w.income > 0 {
        endings.push(Card {
            tone: Tone::Positive,
            text: format!("You received {} {span}.", m(w.income)),
        });
    }
    if w.no_spend_days > 0 {
        let days = if w.no_spend_days == 1 { "day" } else { "days" };
        endings.push(Card {
            tone: Tone::Positive,
            text: format!(
                "You had {} {days} with no flexible spending.",
                w.no_spend_days
            ),
        });
    }
    if endings.is_empty() {
        let n = w.transactions;
        let word = if n == 1 {
            "transaction"
        } else {
            "transactions"
        };
        endings.push(Card {
            tone: Tone::Neutral,
            text: format!("The week of {} had {n} {word}.", short_date(w.week_start)),
        });
    }
    // Keep the last ending (a positive or neutral fact) and fit 5 cards.
    let last = endings.pop().unwrap_or(Card {
        tone: Tone::Neutral,
        text: String::new(),
    });
    let mut cards: Vec<Card> = middle.into_iter().chain(endings).collect();
    cards.truncate(4);
    cards.push(last);
    cards
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Kind;
    use crate::validator::check;
    use proptest::prelude::*;

    fn week() -> WeekSummary {
        WeekSummary {
            week_start: Day::from_ymd(2026, 11, 9).unwrap(),
            partial: false,
            currency: Currency::Usd,
            flex_spent: 21_200,
            history: vec![26_800, 25_000, 30_000, 27_000, 26_000, 28_000],
            top_category: Some(("Groceries".into(), 9_000)),
            recurring: (2, 3_098),
            income: 0,
            no_spend_days: 2,
            transactions: 14,
        }
    }

    #[test]
    fn ends_on_a_positive_fact() {
        let cards = reflection(&week());
        assert!(cards.len() <= 5);
        assert_eq!(
            cards[0].text,
            "You spent $212 on flexible spending in the week of Nov 9."
        );
        assert_eq!(cards.last().unwrap().tone, Tone::Positive);
        assert_eq!(
            cards.last().unwrap().text,
            "You had 2 days with no flexible spending."
        );
    }

    #[test]
    fn higher_weeks_still_end_neutral_or_positive() {
        let mut w = week();
        w.flex_spent = 60_000;
        w.no_spend_days = 0;
        let cards = reflection(&w);
        assert!(cards.iter().any(|c| c.tone == Tone::Higher));
        let last = cards.last().unwrap();
        assert_eq!(last.tone, Tone::Neutral);
        assert_eq!(last.text, "The week of Nov 9 had 14 transactions.");
    }

    proptest! {
        #[test]
        fn any_week_follows_the_rules(
            spent in 0_i64..5_000_000,
            history in proptest::collection::vec(0_i64..5_000_000, 0..10),
            income in 0_i64..1_000_000,
            no_spend in 0_u32..8,
            n in 0_u32..200,
            recurring in 0_u32..5,
            partial in any::<bool>(),
        ) {
            let w = WeekSummary {
                week_start: Day::from_ymd(2026, 11, 9).unwrap(),
                partial,
                currency: Currency::Usd,
                flex_spent: spent,
                history,
                top_category: Some(("Restaurants and coffee".into(), spent)),
                recurring: (recurring, i64::from(recurring) * 1549),
                income,
                no_spend_days: no_spend,
                transactions: n,
            };
            let cards = reflection(&w);
            prop_assert!(!cards.is_empty() && cards.len() <= 5);
            prop_assert_ne!(cards.last().unwrap().tone, Tone::Higher);
            for c in &cards {
                prop_assert_eq!(check(&c.text, Kind::Awareness), vec![], "{}", c.text);
            }
        }
    }
}
