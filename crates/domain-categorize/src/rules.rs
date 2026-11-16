//! Categorization rules (D14): the person's "always set X to Y".
//!
//! A rule matches a merchant or a text in the description, optionally only
//! money in or money out. The rule with the highest priority wins.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Money in or money out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// A positive amount.
    In,
    /// A negative amount.
    Out,
}

/// What a rule matches. Exactly one of `merchant_id` and `name_contains`.
/// Stored as JSON in `app.rules.match`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleMatch {
    /// The normalized merchant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merchant_id: Option<Uuid>,
    /// Text in the description or the merchant name. Not case-sensitive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_contains: Option<String>,
    /// Only money in, or only money out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
}

/// Why a rule is not valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RuleMatchError {
    /// Neither or both of `merchant_id` and `name_contains`.
    #[error("set exactly one of merchant_id and name_contains")]
    OneTarget,
    /// `name_contains` is shorter than 2 or longer than 100 characters.
    #[error("name_contains must have 2 to 100 characters")]
    TextLength,
}

impl RuleMatch {
    /// Checks the shape and trims the text.
    ///
    /// # Errors
    ///
    /// [`RuleMatchError`].
    pub fn validated(mut self) -> Result<Self, RuleMatchError> {
        if let Some(text) = &self.name_contains {
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let len = text.chars().count();
            if !(2..=100).contains(&len) {
                return Err(RuleMatchError::TextLength);
            }
            self.name_contains = Some(text);
        }
        match (&self.merchant_id, &self.name_contains) {
            (Some(_), None) | (None, Some(_)) => Ok(self),
            _ => Err(RuleMatchError::OneTarget),
        }
    }

    /// True if the transaction matches.
    pub fn matches(&self, facts: &Facts<'_>) -> bool {
        let direction = match self.direction {
            None => true,
            Some(Direction::In) => facts.amount_minor > 0,
            Some(Direction::Out) => facts.amount_minor < 0,
        };
        if !direction {
            return false;
        }
        if let Some(id) = self.merchant_id {
            return facts.merchant_id == Some(id);
        }
        if let Some(text) = &self.name_contains {
            let needle = text.to_lowercase();
            let hay = |s: &str| s.to_lowercase().contains(&needle);
            return hay(facts.name) || facts.merchant_name.is_some_and(hay);
        }
        false
    }
}

/// The facts a rule looks at.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    /// The normalized merchant.
    pub merchant_id: Option<Uuid>,
    /// The bank description.
    pub name: &'a str,
    /// The merchant name: the provider's or the normalized one.
    pub merchant_name: Option<&'a str>,
    /// Negative is money out.
    pub amount_minor: i64,
}

/// A rule as the categorizer uses it.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    /// The rule ID.
    pub id: Uuid,
    /// What it matches.
    pub matcher: RuleMatch,
    /// The category it sets.
    pub category_id: Uuid,
}

/// Where an automatic category came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// A person's rule.
    Rule {
        /// The category.
        category_id: Uuid,
        /// The rule.
        rule_id: Uuid,
    },
    /// The provider category mapping.
    Provider {
        /// The category.
        category_id: Uuid,
    },
    /// No category.
    None,
}

/// D14 order: the person's rules (highest priority first), then the provider
/// mapping. `rules` must be sorted by priority, highest first.
pub fn decide(facts: &Facts<'_>, rules: &[CompiledRule], provider: Option<Uuid>) -> Decision {
    if let Some(rule) = rules.iter().find(|r| r.matcher.matches(facts)) {
        return Decision::Rule {
            category_id: rule.category_id,
            rule_id: rule.id,
        };
    }
    provider.map_or(Decision::None, |category_id| Decision::Provider {
        category_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(merchant: Option<Uuid>, name: &str, amount: i64) -> Facts<'_> {
        Facts {
            merchant_id: merchant,
            name,
            merchant_name: None,
            amount_minor: amount,
        }
    }

    #[test]
    fn validates_shape() {
        let m = Uuid::now_v7();
        assert!(
            RuleMatch {
                merchant_id: Some(m),
                ..RuleMatch::default()
            }
            .validated()
            .is_ok()
        );
        assert_eq!(
            RuleMatch::default().validated(),
            Err(RuleMatchError::OneTarget)
        );
        assert_eq!(
            RuleMatch {
                merchant_id: Some(m),
                name_contains: Some("abc".into()),
                direction: None,
            }
            .validated(),
            Err(RuleMatchError::OneTarget)
        );
        assert_eq!(
            RuleMatch {
                name_contains: Some(" x ".into()),
                ..RuleMatch::default()
            }
            .validated(),
            Err(RuleMatchError::TextLength)
        );
        let ok = RuleMatch {
            name_contains: Some("  trader   joe ".into()),
            ..RuleMatch::default()
        }
        .validated()
        .unwrap();
        assert_eq!(ok.name_contains.as_deref(), Some("trader joe"));
    }

    #[test]
    fn rejects_unknown_json_fields() {
        assert!(serde_json::from_str::<RuleMatch>(r#"{"merchant":"x"}"#).is_err());
        let m: RuleMatch =
            serde_json::from_str(r#"{"name_contains":"rent","direction":"out"}"#).unwrap();
        assert_eq!(m.direction, Some(Direction::Out));
    }

    #[test]
    fn matches_merchant_text_and_direction() {
        let m = Uuid::now_v7();
        let by_merchant = RuleMatch {
            merchant_id: Some(m),
            ..RuleMatch::default()
        };
        assert!(by_merchant.matches(&facts(Some(m), "x", -100)));
        assert!(!by_merchant.matches(&facts(Some(Uuid::now_v7()), "x", -100)));
        assert!(!by_merchant.matches(&facts(None, "x", -100)));

        let by_text = RuleMatch {
            name_contains: Some("Venmo".into()),
            direction: Some(Direction::In),
            ..RuleMatch::default()
        };
        assert!(by_text.matches(&facts(None, "VENMO CASHOUT", 5000)));
        assert!(!by_text.matches(&facts(None, "VENMO PAYMENT", -5000)));
        let mut f = facts(None, "TRANSFER", 5000);
        f.merchant_name = Some("Venmo");
        assert!(by_text.matches(&f));
    }

    #[test]
    fn first_matching_rule_wins_then_provider() {
        let (groceries, dining, provider) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let rules = [
            CompiledRule {
                id: Uuid::now_v7(),
                matcher: RuleMatch {
                    name_contains: Some("cafe".into()),
                    ..RuleMatch::default()
                },
                category_id: dining,
            },
            CompiledRule {
                id: Uuid::now_v7(),
                matcher: RuleMatch {
                    name_contains: Some("market".into()),
                    ..RuleMatch::default()
                },
                category_id: groceries,
            },
        ];
        let d = decide(&facts(None, "MARKET CAFE", -1), &rules, Some(provider));
        assert_eq!(
            d,
            Decision::Rule {
                category_id: dining,
                rule_id: rules[0].id
            }
        );
        assert_eq!(
            decide(&facts(None, "SHELL", -1), &rules, Some(provider)),
            Decision::Provider {
                category_id: provider
            }
        );
        assert_eq!(
            decide(&facts(None, "SHELL", -1), &rules, None),
            Decision::None
        );
    }
}
