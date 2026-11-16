//! Plaid request and response shapes, and their conversion to
//! provider-neutral types.

use domain_sync::{
    AggregatorError, ProviderAccount, ProviderBalances, ProviderTransaction, SyncPage,
};
use savespace_core::{Currency, Money, parse_decimal};
use serde::Deserialize;
use serde_json::{Number, Value};

/// A Plaid error body.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct PlaidError {
    pub(crate) error_type: String,
    pub(crate) error_code: String,
    pub(crate) error_message: String,
    pub(crate) request_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LinkTokenResponse {
    pub(crate) link_token: String,
    pub(crate) expiration: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ExchangeResponse {
    pub(crate) access_token: String,
    pub(crate) item_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PublicTokenResponse {
    pub(crate) public_token: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SyncResponse {
    #[serde(default)]
    accounts: Vec<Value>,
    #[serde(default)]
    added: Vec<Value>,
    #[serde(default)]
    modified: Vec<Value>,
    #[serde(default)]
    removed: Vec<RemovedTransaction>,
    next_cursor: String,
    has_more: bool,
}

#[derive(Debug, Deserialize)]
struct RemovedTransaction {
    transaction_id: String,
}

#[derive(Debug, Deserialize)]
struct PlaidAccount {
    account_id: String,
    name: String,
    official_name: Option<String>,
    mask: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    subtype: Option<String>,
    balances: Balances,
}

#[derive(Debug, Deserialize)]
struct Balances {
    current: Option<Number>,
    available: Option<Number>,
    limit: Option<Number>,
    iso_currency_code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlaidTransaction {
    transaction_id: String,
    account_id: String,
    amount: Number,
    iso_currency_code: Option<String>,
    date: String,
    authorized_date: Option<String>,
    name: Option<String>,
    merchant_name: Option<String>,
    pending: bool,
    pending_transaction_id: Option<String>,
    personal_finance_category: Option<Category>,
}

#[derive(Debug, Deserialize)]
struct Category {
    primary: Option<String>,
    detailed: Option<String>,
}

fn invalid(what: &str, e: impl std::fmt::Display) -> AggregatorError {
    AggregatorError::Invalid(format!("{what}: {e}"))
}

fn currency(code: Option<&str>) -> Option<Currency> {
    code.and_then(|c| c.parse().ok())
}

fn minor(value: Option<&Number>, currency: Currency) -> Result<Option<i64>, AggregatorError> {
    value
        .map(|n| {
            parse_decimal(&n.to_string(), currency)
                .map(Money::minor)
                .map_err(|e| invalid("balance", e))
        })
        .transpose()
}

fn date(value: &str) -> Result<jiff::civil::Date, AggregatorError> {
    value.parse().map_err(|e| invalid("date", e))
}

impl SyncResponse {
    /// Converts the response. Accounts and transactions in a currency that
    /// SaveSpace does not support are skipped with a log line.
    pub(crate) fn into_page(self) -> Result<SyncPage, AggregatorError> {
        let mut accounts = Vec::with_capacity(self.accounts.len());
        for raw in self.accounts {
            let a: PlaidAccount = serde_json::from_value(raw).map_err(|e| invalid("account", e))?;
            let Some(cur) = currency(a.balances.iso_currency_code.as_deref()) else {
                tracing::warn!("skipped an account in an unsupported currency");
                continue;
            };
            accounts.push(ProviderAccount {
                provider_account_id: a.account_id,
                name: a.name,
                official_name: a.official_name,
                mask: a.mask,
                kind: a.kind,
                subtype: a.subtype,
                currency: cur,
                balances: ProviderBalances {
                    current: minor(a.balances.current.as_ref(), cur)?,
                    available: minor(a.balances.available.as_ref(), cur)?,
                    limit: minor(a.balances.limit.as_ref(), cur)?,
                },
            });
        }
        Ok(SyncPage {
            accounts,
            added: transactions(self.added)?,
            modified: transactions(self.modified)?,
            removed: self.removed.into_iter().map(|r| r.transaction_id).collect(),
            next_cursor: self.next_cursor,
            has_more: self.has_more,
        })
    }
}

fn transactions(raw: Vec<Value>) -> Result<Vec<ProviderTransaction>, AggregatorError> {
    let mut out = Vec::with_capacity(raw.len());
    for value in raw {
        let t: PlaidTransaction =
            serde_json::from_value(value.clone()).map_err(|e| invalid("transaction", e))?;
        let Some(cur) = currency(t.iso_currency_code.as_deref()) else {
            tracing::warn!("skipped a transaction in an unsupported currency");
            continue;
        };
        // Plaid: positive is money out. SaveSpace: negative is money out.
        let amount = parse_decimal(&t.amount.to_string(), cur)
            .map_err(|e| invalid("amount", e))?
            .checked_neg()
            .map_err(|e| invalid("amount", e))?;
        let category = t.personal_finance_category;
        out.push(ProviderTransaction {
            provider_transaction_id: t.transaction_id,
            provider_account_id: t.account_id,
            amount,
            date: date(&t.date)?,
            authorized_date: t.authorized_date.as_deref().map(date).transpose()?,
            name: t
                .name
                .or_else(|| t.merchant_name.clone())
                .unwrap_or_else(|| "Transaction".to_owned()),
            merchant_name: t.merchant_name,
            pending: t.pending,
            pending_transaction_id: t.pending_transaction_id,
            category_primary: category.as_ref().and_then(|c| c.primary.clone()),
            category_detailed: category.and_then(|c| c.detailed),
            raw: value,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn converts_a_sync_response() {
        let response: SyncResponse = serde_json::from_value(json!({
            "accounts": [{
                "account_id": "acc1", "name": "Plaid Checking", "official_name": null,
                "mask": "0000", "type": "depository", "subtype": "checking",
                "balances": { "current": 110.0, "available": 100.05, "limit": null,
                              "iso_currency_code": "USD" }
            }, {
                "account_id": "acc2", "name": "Crypto", "type": "investment",
                "balances": { "current": 1.5, "iso_currency_code": null,
                              "unofficial_currency_code": "BTC" }
            }],
            "added": [{
                "transaction_id": "t1", "account_id": "acc1", "amount": 89.4,
                "iso_currency_code": "USD", "date": "2026-11-10",
                "authorized_date": "2026-11-09", "name": "SparkFun",
                "merchant_name": "SparkFun", "pending": false,
                "pending_transaction_id": null,
                "personal_finance_category": { "primary": "GENERAL_MERCHANDISE",
                                               "detailed": "GENERAL_MERCHANDISE_OTHER" }
            }, {
                "transaction_id": "t2", "account_id": "acc1", "amount": -500,
                "iso_currency_code": "USD", "date": "2026-11-11", "name": "Payroll",
                "merchant_name": null, "pending": true, "pending_transaction_id": null,
                "personal_finance_category": null
            }],
            "modified": [],
            "removed": [{ "transaction_id": "t0", "account_id": "acc1" }],
            "next_cursor": "c1",
            "has_more": true
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        let page = response.into_page().unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(page.accounts.len(), 1, "BTC account skipped");
        assert_eq!(page.accounts[0].balances.current, Some(11_000));
        assert_eq!(page.accounts[0].balances.available, Some(10_005));
        assert_eq!(page.accounts[0].balances.limit, None);

        let t1 = &page.added[0];
        assert_eq!(t1.amount.minor(), -8940, "money out is negative");
        assert_eq!(t1.date, jiff::civil::date(2026, 11, 10));
        assert_eq!(t1.authorized_date, Some(jiff::civil::date(2026, 11, 9)));
        assert_eq!(t1.category_primary.as_deref(), Some("GENERAL_MERCHANDISE"));
        assert_eq!(t1.raw["transaction_id"], "t1");

        let t2 = &page.added[1];
        assert_eq!(t2.amount.minor(), 50_000, "money in is positive");
        assert!(t2.pending);
        assert_eq!(page.removed, ["t0"]);
        assert_eq!(page.next_cursor, "c1");
        assert!(page.has_more);
    }

    #[test]
    fn rejects_amounts_with_too_many_decimals() {
        let response: SyncResponse = serde_json::from_value(json!({
            "added": [{
                "transaction_id": "t1", "account_id": "acc1", "amount": 1.005,
                "iso_currency_code": "USD", "date": "2026-11-10", "name": "x",
                "pending": false
            }],
            "next_cursor": "c", "has_more": false
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(
            response.into_page(),
            Err(AggregatorError::Invalid(_))
        ));
    }
}
