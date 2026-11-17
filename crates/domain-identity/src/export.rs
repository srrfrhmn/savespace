//! Data export (design doc §12.5): all of the household's data as JSON, and
//! transactions as CSV.
//!
//! Secrets stay out: the household data key, Plaid access tokens, sync
//! cursors, session and recovery code hashes, passkey key material, push
//! subscription keys, and idempotency records. Check-in notes are decrypted,
//! because they are the person's own words.

use platform_crypto::{Keyring, Purpose};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::{Result, SessionInfo};

/// One table and the JSON for one row (`t` is the row).
const TABLES: &[(&str, &str)] = &[
    (
        "household",
        "to_jsonb(t) - 'dek_wrapped' FROM app.households t",
    ),
    ("people", "to_jsonb(t) FROM app.users t"),
    ("members", "to_jsonb(t) FROM app.household_members t"),
    (
        "passkeys",
        "jsonb_build_object('id', t.id, 'name', t.name, 'created_at', t.created_at, \
         'last_used_at', t.last_used_at) FROM app.passkeys t",
    ),
    (
        "connections",
        "to_jsonb(t) - 'access_token_enc' - 'sync_cursor' FROM app.connections t",
    ),
    ("accounts", "to_jsonb(t) FROM app.accounts t"),
    ("balances", "to_jsonb(t) FROM app.balances t"),
    ("transactions", "to_jsonb(t) FROM app.transactions t"),
    (
        "transactions_raw",
        "to_jsonb(t) FROM app.transactions_raw t",
    ),
    ("edits", "to_jsonb(t) FROM app.user_overrides t"),
    ("splits", "to_jsonb(t) FROM app.transaction_splits t"),
    ("categories", "to_jsonb(t) FROM app.categories t"),
    ("merchants", "to_jsonb(t) FROM app.merchants t"),
    ("rules", "to_jsonb(t) FROM app.rules t"),
    ("budgets", "to_jsonb(t) FROM app.budgets t"),
    ("recurring", "to_jsonb(t) FROM app.recurring_series t"),
    ("insights", "to_jsonb(t) FROM app.insights t"),
    (
        "insight_feedback",
        "to_jsonb(t) FROM app.insight_feedback t",
    ),
    ("insight_mutes", "to_jsonb(t) FROM app.insight_mutes t"),
    ("insight_reviews", "to_jsonb(t) FROM app.insight_reviews t"),
    ("notifications", "to_jsonb(t) FROM app.notifications t"),
    ("audit_log", "to_jsonb(t) FROM app.audit_log t"),
];

/// Household tables the export leaves out, because they hold only secrets or
/// internal state. `check_ins` is exported separately (notes are decrypted).
pub const NOT_EXPORTED: &[&str] = &[
    "idempotency_keys",
    "outbox",
    "push_subscriptions",
    "recovery_codes",
    "sessions",
];

/// True if the export includes the table `app.{table}`.
pub fn covers(table: &str) -> bool {
    table == "check_ins"
        || TABLES
            .iter()
            .any(|(_, select)| select.ends_with(&format!("FROM app.{table} t")))
}

/// An export.
#[derive(Debug, Clone)]
pub struct Export {
    /// Every table, by name.
    pub data: Value,
    /// Transactions as the person sees them, as CSV.
    pub transactions_csv: String,
}

/// Quotes one CSV field (RFC 4180). A leading `=`, `+`, `-`, or `@` gets a
/// `'` first, so a spreadsheet does not run it as a formula.
fn csv_field(value: &str) -> String {
    let guarded = if value.starts_with(['=', '+', '-', '@']) && value.parse::<f64>().is_err() {
        format!("'{value}")
    } else {
        value.to_owned()
    };
    if guarded.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", guarded.replace('"', "\"\""))
    } else {
        guarded
    }
}

type CsvRow = (
    jiff_sqlx::Date,
    String,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
    bool,
);

type CheckInRow = (
    Uuid,
    i16,
    Option<Vec<u8>>,
    jiff_sqlx::Date,
    jiff_sqlx::Timestamp,
);

/// Builds the export for the session's household.
///
/// # Errors
///
/// A database or decryption error.
pub async fn export(
    pool: &sqlx::PgPool,
    session: &SessionInfo,
    keyring: Option<&Keyring>,
) -> Result<Export> {
    let mut tx = platform_db::tenant_tx(pool, session.household_id).await?;
    let mut data = Map::new();
    for (name, select) in TABLES {
        let sql =
            format!("SELECT coalesce(jsonb_agg(r.x), '[]'::jsonb) FROM (SELECT {select}) AS r(x)");
        // The SQL is built from the constant table list above only.
        let rows: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .fetch_one(&mut *tx)
            .await?;
        data.insert((*name).to_owned(), rows);
    }

    let check_ins: Vec<CheckInRow> = sqlx::query_as(
        "SELECT id, score, note_enc, local_date, created_at FROM app.check_ins ORDER BY created_at",
    )
    .fetch_all(&mut *tx)
    .await?;
    let dek = match keyring {
        Some(k) if check_ins.iter().any(|c| c.2.is_some()) => {
            Some(k.dek(&mut tx, session.household_id).await?)
        }
        _ => None,
    };
    let mut items = Vec::with_capacity(check_ins.len());
    for (id, score, note_enc, date, created_at) in check_ins {
        let note = match (note_enc, &dek) {
            (Some(enc), Some(dek)) => Some(
                String::from_utf8_lossy(&dek.open(Purpose::Note, session.household_id, &enc)?)
                    .into_owned(),
            ),
            (Some(_), None) => Some("(encrypted; not readable on this server)".to_owned()),
            (None, _) => None,
        };
        items.push(serde_json::json!({
            "id": id,
            "score": score,
            "note": note,
            "date": date.to_jiff().to_string(),
            "created_at": created_at.to_jiff().to_string(),
        }));
    }
    data.insert("check_ins".to_owned(), Value::from(items));

    let rows: Vec<CsvRow> = sqlx::query_as(
        "SELECT t.date, a.name, t.amount_minor, t.currency, t.name, \
           coalesce(o.merchant_name, m.name, t.merchant_name), c.name, o.note, t.pending, \
           coalesce(o.hidden, false) \
         FROM app.transactions t JOIN app.accounts a ON a.id = t.account_id \
         LEFT JOIN app.user_overrides o ON o.transaction_id = t.id \
         LEFT JOIN app.merchants m ON m.id = t.merchant_id \
         LEFT JOIN app.categories c ON c.id = coalesce(o.category_id, t.category_id) \
         ORDER BY t.date, t.id",
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut csv = String::from(
        "date,account,amount_minor,currency,description,merchant,category,note,pending,hidden\r\n",
    );
    for (date, account, amount, currency, name, merchant, category, note, pending, hidden) in rows {
        let fields = [
            date.to_jiff().to_string(),
            csv_field(&account),
            amount.to_string(),
            currency,
            csv_field(&name),
            csv_field(merchant.as_deref().unwrap_or("")),
            csv_field(category.as_deref().unwrap_or("")),
            csv_field(note.as_deref().unwrap_or("")),
            pending.to_string(),
            hidden.to_string(),
        ];
        csv.push_str(&fields.join(","));
        csv.push_str("\r\n");
    }
    Ok(Export {
        data: Value::Object(data),
        transactions_csv: csv,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_csv_and_blocks_formulas() {
        assert_eq!(csv_field("Blue Bottle"), "Blue Bottle");
        assert_eq!(csv_field("Joe's, Inc"), "\"Joe's, Inc\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("=HYPERLINK(\"x\")"), "\"'=HYPERLINK(\"\"x\"\")\"");
        assert_eq!(csv_field("@SUM(A1)"), "'@SUM(A1)");
        assert_eq!(csv_field("-12"), "-12");
    }
}
