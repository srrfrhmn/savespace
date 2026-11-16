//! The money check-in (design doc §6.1): "How do you feel about money
//! today?" 1 to 5, with an optional note. The note is encrypted with the
//! household data key (`platform-crypto`, purpose `Note`, §9.1).

use platform_crypto::{Keyring, Purpose};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::facts;
use crate::service::{InsightError, Result};

/// The longest note, in characters.
pub const MAX_NOTE: usize = 500;

/// A check-in.
#[derive(Debug, Clone, Serialize)]
pub struct CheckIn {
    /// The check-in ID.
    pub id: Uuid,
    /// 1 to 5.
    pub score: i16,
    /// The note, decrypted.
    pub note: Option<String>,
    /// The day in the person's time zone.
    pub date: jiff::civil::Date,
    /// When it was saved.
    pub created_at: jiff::Timestamp,
}

/// Check-ins.
#[derive(Debug, Clone)]
pub struct CheckInService {
    pool: PgPool,
    keyring: Option<Keyring>,
}

type Row = (
    Uuid,
    i16,
    Option<Vec<u8>>,
    jiff_sqlx::Date,
    jiff_sqlx::Timestamp,
);

impl CheckInService {
    /// Creates the service. Without a keyring, notes are not accepted.
    pub const fn new(pool: PgPool, keyring: Option<Keyring>) -> Self {
        Self { pool, keyring }
    }

    /// Saves a check-in. `score` must be 1 to 5 and the note at most
    /// [`MAX_NOTE`] characters (the caller checks both).
    ///
    /// # Errors
    ///
    /// [`InsightError::NotesUnavailable`] for a note without a keyring, an
    /// encryption error, or a database error.
    pub async fn create(
        &self,
        household_id: Uuid,
        user_id: Uuid,
        score: i16,
        note: Option<&str>,
    ) -> Result<CheckIn> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let note_enc = match note {
            Some(text) => {
                let keyring = self
                    .keyring
                    .as_ref()
                    .ok_or(InsightError::NotesUnavailable)?;
                let dek = keyring.dek(&mut tx, household_id).await?;
                Some(dek.seal(Purpose::Note, household_id, text.as_bytes()))
            }
            None => None,
        };
        let today = facts::date_of(facts::today(&mut tx).await?);
        let (id, created_at): (Uuid, jiff_sqlx::Timestamp) = sqlx::query_as(
            "INSERT INTO app.check_ins (household_id, user_id, score, note_enc, local_date) \
             VALUES ($1, $2, $3, $4, $5) RETURNING id, created_at",
        )
        .bind(household_id)
        .bind(user_id)
        .bind(score)
        .bind(note_enc)
        .bind(jiff_sqlx::Date::from(today))
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(CheckIn {
            id,
            score,
            note: note.map(str::to_owned),
            date: today,
            created_at: created_at.to_jiff(),
        })
    }

    /// The person's latest check-ins, newest first.
    ///
    /// # Errors
    ///
    /// An encryption error or a database error.
    pub async fn list(
        &self,
        household_id: Uuid,
        user_id: Uuid,
        limit: i64,
    ) -> Result<Vec<CheckIn>> {
        let mut tx = platform_db::tenant_tx(&self.pool, household_id).await?;
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, score, note_enc, local_date, created_at FROM app.check_ins \
             WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(user_id)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        let dek = match (&self.keyring, rows.iter().any(|r| r.2.is_some())) {
            (Some(k), true) => Some(k.dek(&mut tx, household_id).await?),
            _ => None,
        };
        tx.commit().await?;
        rows.into_iter()
            .map(|(id, score, note_enc, date, created_at)| {
                let note = match (note_enc, &dek) {
                    (Some(enc), Some(dek)) => {
                        let plain = dek.open(Purpose::Note, household_id, &enc)?;
                        Some(String::from_utf8_lossy(&plain).into_owned())
                    }
                    _ => None,
                };
                Ok(CheckIn {
                    id,
                    score,
                    note,
                    date: date.to_jiff(),
                    created_at: created_at.to_jiff(),
                })
            })
            .collect()
    }
}
