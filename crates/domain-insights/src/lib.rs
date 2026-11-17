//! `domain-insights`: the fact builder, the insight rule registry, the view
//! policy, the copy validator, the weekly reflection, and check-ins (design
//! doc §7, §6.1).
//!
//! Pipeline (§7.2) as built in Phase 1:
//!
//! 1. [`facts`] reads the household's data into a [`rules::FactSet`].
//! 2. Each rule in [`rules::registry`] returns candidates. Rule kill switches
//!    (`app.insight_rule_flags`) and the person's mutes remove some rules.
//! 3. [`service::InsightService::generate`] stores new candidates, refreshes
//!    current ones, skips a rule and scope that was shown in the last 7
//!    days, and expires insights whose condition no longer holds.
//! 4. [`policy::select`] picks what a view shows.
//!
//! Text comes from templates only (D10). [`validator`] checks every template
//! in tests.

pub mod checkin;
pub mod facts;
pub mod model;
pub mod policy;
pub mod reflection;
pub mod review;
pub mod rules;
pub mod service;
pub mod validator;

pub use model::{Candidate, Fact, Kind};
pub use service::{InsightError, InsightService, StoredInsight};

/// The job kind that runs the pipeline for one household.
pub const GENERATE_JOB: &str = "insights.household";

/// The hourly job that starts the daily 06:00 run for each household.
pub const DAILY_JOB: &str = "insights.daily";

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use savespace_core::recurring::Cadence;
    use savespace_core::{Currency, Day};
    use uuid::Uuid;

    use crate::model::Kind;
    use crate::rules::{
        BalanceFacts, FactSet, MonthFacts, SeriesCharge, Upcoming, WeekFacts, registry,
    };
    use crate::validator::check;

    fn day(y: i32, m: u8, d: u8) -> Day {
        Day::from_ymd(y, m, d).unwrap()
    }

    fn empty() -> FactSet {
        FactSet {
            today: day(2026, 11, 18),
            currency: Currency::Usd,
            week: None,
            month: None,
            upcoming: Vec::new(),
            recent_charges: Vec::new(),
            balance: None,
        }
    }

    fn run(f: &FactSet) -> Vec<crate::Candidate> {
        registry().iter().flat_map(|r| r.evaluate(f)).collect()
    }

    fn ids(f: &FactSet) -> Vec<&'static str> {
        run(f).iter().map(|c| c.rule_id).collect()
    }

    #[test]
    fn no_facts_no_insights() {
        assert_eq!(ids(&empty()), Vec::<&str>::new());
    }

    #[test]
    fn week_below_and_above_usual_need_six_weeks() {
        let mut f = empty();
        f.week = Some(WeekFacts {
            week_start: day(2026, 11, 16),
            spent: 21_200,
            history: vec![26_800; 5],
        });
        assert_eq!(ids(&f), Vec::<&str>::new(), "5 weeks is not enough");
        f.week.as_mut().unwrap().history.push(26_800);
        let c = run(&f);
        assert_eq!(c.len(), 1);
        assert_eq!(
            c[0].text,
            "You spent less than usual this week: $212 so far, compared to a usual $268."
        );
        f.week.as_mut().unwrap().spent = 26_000;
        assert_eq!(ids(&f), Vec::<&str>::new(), "within the threshold");
        f.week.as_mut().unwrap().spent = 40_000;
        assert_eq!(ids(&f), ["spend.week_above_usual"]);
    }

    #[test]
    fn on_track_uses_a_linear_pace() {
        let mut f = empty();
        let month = MonthFacts {
            month_start: day(2026, 11, 1),
            day: 15,
            days: 30,
            plan: 60_000,
            spent: 31_000,
            left: 29_000,
        };
        f.month = Some(month);
        let c = run(&f);
        assert_eq!(
            c[0].text,
            "You are on track for your monthly budget. $290 left for 16 days."
        );
        f.month = Some(MonthFacts {
            spent: 32_000,
            left: 28_000,
            ..month
        });
        assert_eq!(ids(&f), Vec::<&str>::new(), "more than 5% over the pace");
    }

    #[test]
    fn recurring_charge_expected_or_higher() {
        let mut f = empty();
        let base = SeriesCharge {
            transaction_id: Uuid::now_v7(),
            merchant: "Netflix".into(),
            amount: -1549,
            date: day(2026, 11, 15),
            previous: Some(day(2026, 10, 15)),
            typical: -1549,
            spread: 0,
            cadence: Cadence::Monthly,
            confidence_bp: 10_000,
        };
        f.recent_charges = vec![base.clone()];
        let c = run(&f);
        assert_eq!(
            c[0].text,
            "This charge is recurring and expected. Netflix charges about $15.49 each month."
        );
        assert_eq!(c[0].transaction_id, Some(base.transaction_id));
        f.recent_charges = vec![SeriesCharge {
            amount: -2299,
            ..base.clone()
        }];
        assert_eq!(
            run(&f)[0].text,
            "This Netflix charge is $7.50 higher than usual."
        );
        f.recent_charges = vec![SeriesCharge {
            date: day(2026, 11, 25),
            ..base.clone()
        }];
        assert_eq!(ids(&f), Vec::<&str>::new(), "too late to call it expected");
        f.recent_charges = vec![SeriesCharge {
            confidence_bp: 7000,
            ..base
        }];
        assert_eq!(ids(&f), Vec::<&str>::new(), "not confident enough");
    }

    #[test]
    fn upcoming_and_may_not_cover() {
        let mut f = empty();
        let rent = Upcoming {
            series_id: Uuid::now_v7(),
            merchant: "Oak Apartments".into(),
            amount: 180_000,
            date: day(2026, 11, 20),
        };
        f.upcoming = vec![rent.clone()];
        f.balance = Some(BalanceFacts {
            cash: 250_000,
            bills: vec![rent.clone()],
        });
        assert_eq!(
            run(&f)[0].text,
            "Oak Apartments ($1,800) is expected on Nov 20. Your cash covers it."
        );
        f.balance = Some(BalanceFacts {
            cash: 100_000,
            bills: vec![rent],
        });
        let c = run(&f);
        assert_eq!(
            c.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            [
                "Oak Apartments ($1,800) is expected on Nov 20.",
                "Your cash may not cover Oak Apartments ($1,800) on Nov 20. You can review your upcoming bills."
            ]
        );
        assert_eq!(c[1].kind, Kind::Concern);
    }

    /// Realistic merchant names, including a long one.
    fn merchants() -> impl Strategy<Value = String> {
        proptest::sample::select(vec![
            "Netflix",
            "Oak Apartments",
            "Trader Joe's",
            "PG&E",
            "Automatic Payment Thank",
            "The Very Long Name Of A Regional Utility Company Incorporated",
            "McDonald's",
        ])
        .prop_map(str::to_owned)
    }

    fn fact_sets() -> impl Strategy<Value = FactSet> {
        (
            0_i64..20_000_000,
            proptest::collection::vec(0_i64..20_000_000, 0..12),
            (1_u8..=31, 0_i64..50_000_000, 0_i64..50_000_000),
            proptest::collection::vec((merchants(), 1_i64..100_000_000, 0_i32..7), 0..5),
            (
                -10_000_000_i64..100_000_000,
                proptest::option::of(0_i64..40),
            ),
        )
            .prop_map(
                |(spent, history, (d, plan, mspent), bills, (cash, shift))| {
                    let today = day(2026, 11, 18);
                    let upcoming: Vec<Upcoming> = bills
                        .into_iter()
                        .map(|(merchant, amount, offset)| Upcoming {
                            series_id: Uuid::nil(),
                            merchant,
                            amount,
                            date: today.plus_days(offset),
                        })
                        .collect();
                    FactSet {
                        today,
                        currency: Currency::Usd,
                        week: Some(WeekFacts {
                            week_start: day(2026, 11, 16),
                            spent,
                            history,
                        }),
                        month: Some(MonthFacts {
                            month_start: day(2026, 11, 1),
                            day: d.min(30),
                            days: 30,
                            plan,
                            spent: mspent,
                            left: plan - mspent,
                        }),
                        recent_charges: upcoming
                            .iter()
                            .map(|u| SeriesCharge {
                                transaction_id: Uuid::nil(),
                                merchant: u.merchant.clone(),
                                amount: -u.amount - shift.unwrap_or(0) * 1000,
                                date: today,
                                previous: Some(today.plus_months(-1)),
                                typical: -u.amount,
                                spread: 0,
                                cadence: Cadence::Monthly,
                                confidence_bp: 9000,
                            })
                            .collect(),
                        balance: Some(BalanceFacts {
                            cash,
                            bills: upcoming.clone(),
                        }),
                        upcoming,
                    }
                },
            )
    }

    proptest! {
        /// Every text any rule can produce passes the copy checks (Appendix B).
        #[test]
        fn every_template_passes_the_validator(f in fact_sets()) {
            for c in run(&f) {
                prop_assert_eq!(check(&c.text, c.kind), vec![], "{}: {}", c.rule_id, c.text);
                prop_assert!((0..=10_000).contains(&c.confidence_bp));
                prop_assert!((0..=10_000).contains(&c.relevance_bp));
                prop_assert!(!c.facts.is_empty());
            }
        }

        /// Reassurance and the matching awareness never both fire.
        #[test]
        fn below_and_above_usual_exclude_each_other(f in fact_sets()) {
            let ids: Vec<_> = run(&f).iter().map(|c| c.rule_id).collect();
            prop_assert!(!(ids.contains(&"spend.week_below_usual") && ids.contains(&"spend.week_above_usual")));
        }

        /// The view never shows more than 3, besides extra concerns, and never
        /// a reassurance under a concern for the same scope.
        #[test]
        fn views_follow_the_policy(f in fact_sets()) {
            let picked = crate::policy::select(run(&f));
            let concerns = picked.iter().filter(|c| c.kind == Kind::Concern).count();
            prop_assert!(picked.len() <= 3.max(concerns));
            for c in picked.iter().filter(|c| c.kind == Kind::Reassurance) {
                prop_assert!(!picked.iter().any(|x| x.kind == Kind::Concern && x.scope == c.scope));
            }
        }
    }
}
