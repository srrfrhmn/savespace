//! The API idempotency store (design doc §11.3).
//!
//! The first request with a key reserves it. Later requests with the same
//! key and the same request hash get the stored response. A different hash
//! is an error. Rows expire after 24 hours (`app.delete_expired`).

use sqlx::PgPool;
use uuid::Uuid;

use crate::tenant_tx;

/// The result of [`begin`].
#[derive(Debug, PartialEq, Eq)]
pub enum Begin {
    /// The key is new and now reserved. Run the request, then call
    /// [`finish`] or [`abandon`].
    New,
    /// A request with this key finished. Return this response.
    Replay {
        /// The HTTP status.
        status: u16,
        /// The response body.
        body: Vec<u8>,
        /// The `Content-Type` header, if any.
        content_type: Option<String>,
    },
    /// A request with this key is still running.
    InProgress,
    /// The key was used for a different request.
    Mismatch,
}

/// Reserves a key, or reports what an earlier request with it did.
///
/// # Errors
///
/// Returns a database error.
pub async fn begin(
    pool: &PgPool,
    household_id: Uuid,
    key: &str,
    request_hash: &[u8],
) -> Result<Begin, sqlx::Error> {
    let mut tx = tenant_tx(pool, household_id).await?;
    let inserted = sqlx::query(
        "INSERT INTO app.idempotency_keys (household_id, key, request_hash) \
         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(household_id)
    .bind(key)
    .bind(request_hash)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if inserted {
        tx.commit().await?;
        return Ok(Begin::New);
    }
    let row: (Vec<u8>, Option<i16>, Option<Vec<u8>>, Option<String>) = sqlx::query_as(
        "SELECT request_hash, status, response, content_type FROM app.idempotency_keys \
         WHERE household_id = $1 AND key = $2",
    )
    .bind(household_id)
    .bind(key)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let (hash, status, body, content_type) = row;
    Ok(if hash != request_hash {
        Begin::Mismatch
    } else if let Some(status) = status {
        Begin::Replay {
            status: u16::try_from(status).unwrap_or(500),
            body: body.unwrap_or_default(),
            content_type,
        }
    } else {
        Begin::InProgress
    })
}

/// Stores the response for a reserved key.
///
/// # Errors
///
/// Returns a database error.
pub async fn finish(
    pool: &PgPool,
    household_id: Uuid,
    key: &str,
    status: u16,
    body: &[u8],
    content_type: Option<&str>,
) -> Result<(), sqlx::Error> {
    let mut tx = tenant_tx(pool, household_id).await?;
    sqlx::query(
        "UPDATE app.idempotency_keys SET status = $3, response = $4, content_type = $5 \
         WHERE household_id = $1 AND key = $2",
    )
    .bind(household_id)
    .bind(key)
    .bind(i16::try_from(status).unwrap_or(500))
    .bind(body)
    .bind(content_type)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Releases a reserved key after a server error, so a retry can run.
///
/// # Errors
///
/// Returns a database error.
pub async fn abandon(pool: &PgPool, household_id: Uuid, key: &str) -> Result<(), sqlx::Error> {
    let mut tx = tenant_tx(pool, household_id).await?;
    sqlx::query("DELETE FROM app.idempotency_keys WHERE household_id = $1 AND key = $2")
        .bind(household_id)
        .bind(key)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}
