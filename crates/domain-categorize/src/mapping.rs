//! Provider category to SaveSpace category (D14: the fallback after the
//! person's rules).
//!
//! Plaid sends a personal finance category: a primary value such as
//! `FOOD_AND_DRINK` and a detailed value such as `FOOD_AND_DRINK_GROCERIES`.
//! The result is a seeded category key (migration `…_categorize.sql`).

/// Detailed values that map to another category than their primary value.
const DETAILED: &[(&str, &str)] = &[
    ("FOOD_AND_DRINK_GROCERIES", "groceries"),
    ("RENT_AND_UTILITIES_RENT", "housing"),
    ("RENT_AND_UTILITIES_INTERNET_AND_CABLE", "phone_internet"),
    ("RENT_AND_UTILITIES_TELEPHONE", "phone_internet"),
    ("LOAN_PAYMENTS_MORTGAGE_PAYMENT", "housing"),
    ("LOAN_PAYMENTS_CREDIT_CARD_PAYMENT", "transfer"),
    ("GENERAL_SERVICES_INSURANCE", "insurance"),
    ("GENERAL_SERVICES_CHILDCARE", "childcare"),
    ("GENERAL_SERVICES_EDUCATION", "education"),
    ("GENERAL_SERVICES_AUTOMOTIVE", "transport"),
    ("GOVERNMENT_AND_NON_PROFIT_DONATIONS", "giving"),
    ("GOVERNMENT_AND_NON_PROFIT_TAX_PAYMENT", "taxes"),
];

/// Primary values.
const PRIMARY: &[(&str, &str)] = &[
    ("INCOME", "income"),
    ("TRANSFER_IN", "transfer"),
    ("TRANSFER_OUT", "transfer"),
    ("LOAN_PAYMENTS", "loan_payments"),
    ("BANK_FEES", "fees"),
    ("ENTERTAINMENT", "entertainment"),
    ("FOOD_AND_DRINK", "dining"),
    ("GENERAL_MERCHANDISE", "shopping"),
    ("HOME_IMPROVEMENT", "home"),
    ("MEDICAL", "health"),
    ("PERSONAL_CARE", "personal_care"),
    ("GENERAL_SERVICES", "other"),
    ("GOVERNMENT_AND_NON_PROFIT", "other"),
    ("TRANSPORTATION", "transport"),
    ("TRAVEL", "travel"),
    ("RENT_AND_UTILITIES", "utilities"),
];

/// The category key for a provider category, if one fits.
pub fn provider_category(primary: Option<&str>, detailed: Option<&str>) -> Option<&'static str> {
    let find = |table: &[(&str, &'static str)], value: &str| {
        table.iter().find(|(k, _)| *k == value).map(|(_, v)| *v)
    };
    detailed
        .and_then(|d| find(DETAILED, d))
        .or_else(|| primary.and_then(|p| find(PRIMARY, p)))
}

/// Every category key the mapping can return. Each one must be seeded.
pub fn mapped_keys() -> impl Iterator<Item = &'static str> {
    DETAILED.iter().chain(PRIMARY).map(|(_, v)| *v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detailed_wins_over_primary() {
        assert_eq!(
            provider_category(Some("FOOD_AND_DRINK"), Some("FOOD_AND_DRINK_GROCERIES")),
            Some("groceries")
        );
        assert_eq!(
            provider_category(Some("FOOD_AND_DRINK"), Some("FOOD_AND_DRINK_COFFEE")),
            Some("dining")
        );
        assert_eq!(
            provider_category(
                Some("LOAN_PAYMENTS"),
                Some("LOAN_PAYMENTS_CREDIT_CARD_PAYMENT")
            ),
            Some("transfer")
        );
    }

    #[test]
    fn unknown_or_missing_is_none() {
        assert_eq!(provider_category(None, None), None);
        assert_eq!(provider_category(Some("SOMETHING_NEW"), None), None);
        // Plaid's OTHER means it does not know. "No category" lets the
        // person find these and set one.
        assert_eq!(provider_category(Some("OTHER"), Some("OTHER_OTHER")), None);
        assert_eq!(provider_category(None, Some("MEDICAL_DENTAL_CARE")), None);
        assert_eq!(
            provider_category(Some("MEDICAL"), Some("MEDICAL_DENTAL_CARE")),
            Some("health")
        );
    }
}
