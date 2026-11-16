//! Data rights (design doc §12.5): export everything, and delete the account.

use std::io::Write as _;

use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use zip::write::SimpleFileOptions;

use crate::AppState;
use crate::error::{ApiError, ErrorBody};
use crate::session::{Auth, clear_cookie};

/// Routes that need a session.
pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_export))
        .routes(routes!(delete_account))
}

const README: &str = "SaveSpace data export\r\n\
\r\n\
data.json        Everything SaveSpace stores about you, by table.\r\n\
transactions.csv Your transactions with your categories, notes, and edits.\r\n\
\r\n\
Amounts are in minor units (cents for USD). Negative is money out.\r\n\
Secrets are not included: bank access tokens, encryption keys, session and\r\n\
recovery code hashes, and passkey keys.\r\n";

/// Builds the zip: `README.txt`, `data.json`, `transactions.csv`.
fn zip_export(export: &domain_identity::export::Export) -> Result<Vec<u8>, zip::result::ZipError> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut buffer);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let json = serde_json::to_vec_pretty(&export.data).unwrap_or_default();
    for (name, bytes) in [
        ("README.txt", README.as_bytes()),
        ("data.json", json.as_slice()),
        ("transactions.csv", export.transactions_csv.as_bytes()),
    ] {
        zip.start_file(name, options)?;
        zip.write_all(bytes)?;
    }
    zip.finish()?;
    Ok(buffer.into_inner())
}

/// Downloads all of the household's data as a zip of JSON and CSV. Needs
/// step-up.
#[utoipa::path(post, path = "/exports", tag = "me",
    responses((status = 200, content_type = "application/zip", body = Vec<u8>),
              (status = "4XX", body = ErrorBody)))]
async fn create_export(State(state): State<AppState>, auth: Auth) -> Result<Response, ApiError> {
    auth.require_step_up()?;
    let export =
        domain_identity::export::export(&state.db, &auth.session, state.keyring.as_ref()).await?;
    let bytes = zip_export(&export).map_err(|e| {
        tracing::error!(error = %e, "export zip");
        ApiError::internal()
    })?;
    let date = jiff::Timestamp::now()
        .to_zoned_datetime(jiff::tz::TimeZone::UTC)
        .date();
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"savespace-export-{date}.zip\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        bytes,
    )
        .into_response())
}

/// Deletes the account and all data (design doc §12.5). Needs step-up.
/// Removes each bank connection at Plaid first. Then deletes the household,
/// which also deletes the household data key, so encrypted values in backups
/// cannot be read. Backups expire in 35 days.
#[utoipa::path(delete, path = "/me", tag = "me",
    responses((status = 204), (status = "4XX", body = ErrorBody), (status = 503, body = ErrorBody)))]
async fn delete_account(State(state): State<AppState>, auth: Auth) -> Result<Response, ApiError> {
    auth.require_step_up()?;
    let household = auth.session.household_id;
    // Without Plaid on this server, no connection can exist to remove.
    if let Some(sync) = state.sync.as_ref() {
        sync.remove_all_items(household).await?;
    }
    state
        .identity
        .delete_household(&auth.session, auth.ctx.request_id.as_deref())
        .await?;
    if let Some(keyring) = &state.keyring {
        keyring.forget(household);
    }
    tracing::info!("household deleted");
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, clear_cookie());
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zips_three_files() {
        let export = domain_identity::export::Export {
            data: serde_json::json!({ "accounts": [] }),
            transactions_csv: "date\r\n".into(),
        };
        let bytes = zip_export(&export).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().unwrap().into_owned())
            .collect();
        assert_eq!(names, ["README.txt", "data.json", "transactions.csv"]);
    }
}
