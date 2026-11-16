//! Plaid client tests against a mock server (wiremock).

#![expect(clippy::unwrap_used, reason = "tests")]

use aggregator_plaid::{PlaidClient, PlaidConfig, PlaidEnv, WebhookError};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use domain_sync::{Aggregator, AggregatorError, LinkTokenRequest};
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::json;
use sha2::{Digest, Sha256};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[expect(
    clippy::unused_async,
    reason = "reads like the other awaited setup calls"
)]
async fn client(server: &MockServer) -> PlaidClient {
    PlaidClient::new(PlaidConfig {
        env: PlaidEnv::Sandbox,
        client_id: "client-1".into(),
        secret: "secret-1".into(),
        base_url: Some(server.uri()),
    })
    .unwrap()
}

#[tokio::test]
async fn link_token_sends_credentials_products_and_webhook() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/link/token/create"))
        .and(header("PLAID-CLIENT-ID", "client-1"))
        .and(header("PLAID-SECRET", "secret-1"))
        .and(header("Plaid-Version", "2020-09-14"))
        .and(body_partial_json(json!({
            "client_name": "SaveSpace",
            "products": ["transactions"],
            "user": { "client_user_id": "usr_1" },
            "webhook": "https://app.test/v1/webhooks/plaid",
            "transactions": { "days_requested": 730 }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "link_token": "link-sandbox-abc", "expiration": "2026-11-17T12:00:00Z",
            "request_id": "r1"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let token = client(&server)
        .await
        .create_link_token(&LinkTokenRequest {
            client_user_id: "usr_1".into(),
            webhook_url: Some("https://app.test/v1/webhooks/plaid".into()),
            update_access_token: None,
        })
        .await
        .unwrap();
    assert_eq!(token.link_token, "link-sandbox-abc");
}

#[tokio::test]
async fn sync_maps_errors() {
    let server = MockServer::start().await;
    Mock::given(path("/transactions/sync"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error_type": "ITEM_ERROR", "error_code": "ITEM_LOGIN_REQUIRED",
            "error_message": "the login details of this item have changed",
            "display_message": null, "request_id": "r2"
        })))
        .mount(&server)
        .await;
    let result = client(&server).await.sync_page("access-1", None).await;
    assert!(
        matches!(result, Err(AggregatorError::LoginRequired { .. })),
        "{result:?}"
    );
}

#[tokio::test]
async fn sync_parses_a_page_with_cursor() {
    let server = MockServer::start().await;
    Mock::given(path("/transactions/sync"))
        .and(body_partial_json(
            json!({ "access_token": "access-1", "cursor": "c0", "count": 500 }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "accounts": [], "added": [{
                "transaction_id": "t1", "account_id": "a1", "amount": 12.5,
                "iso_currency_code": "USD", "date": "2026-11-01", "name": "Cafe",
                "pending": false
            }],
            "modified": [], "removed": [], "next_cursor": "c1", "has_more": false,
            "transactions_update_status": "HISTORICAL_UPDATE_COMPLETE", "request_id": "r"
        })))
        .mount(&server)
        .await;
    let page = client(&server)
        .await
        .sync_page("access-1", Some("c0"))
        .await
        .unwrap();
    assert_eq!(page.added[0].amount.minor(), -1250);
    assert_eq!(page.next_cursor, "c1");
}

fn signing_key() -> SigningKey {
    SigningKey::from_slice(&[7_u8; 32]).unwrap()
}

/// A JWK response for the test key.
fn jwk_response(expired: bool) -> serde_json::Value {
    let point = signing_key().verifying_key().to_sec1_point(false);
    let bytes = point.as_bytes();
    json!({
        "key": {
            "alg": "ES256", "crv": "P-256", "kid": "kid-1", "kty": "EC", "use": "sig",
            "x": URL_SAFE_NO_PAD.encode(&bytes[1..33]),
            "y": URL_SAFE_NO_PAD.encode(&bytes[33..65]),
            "created_at": 1_700_000_000,
            "expired_at": if expired { json!(1_800_000_000) } else { json!(null) }
        },
        "request_id": "r"
    })
}

fn jwt(alg: &str, iat: i64, body: &[u8]) -> String {
    let header =
        URL_SAFE_NO_PAD.encode(json!({ "alg": alg, "kid": "kid-1", "typ": "JWT" }).to_string());
    let claims = URL_SAFE_NO_PAD.encode(
        json!({ "iat": iat, "request_body_sha256": hex::encode(Sha256::digest(body)) }).to_string(),
    );
    let signing_input = format!("{header}.{claims}");
    let signature: Signature = signing_key().sign(signing_input.as_bytes());
    format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

#[tokio::test]
async fn verifies_webhooks() {
    let server = MockServer::start().await;
    Mock::given(path("/webhook_verification_key/get"))
        .and(body_partial_json(json!({ "key_id": "kid-1" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwk_response(false)))
        // The key is cached after the first fetch.
        .expect(1)
        .mount(&server)
        .await;
    let plaid = client(&server).await;
    let body = br#"{
  "webhook_type": "TRANSACTIONS",
  "webhook_code": "SYNC_UPDATES_AVAILABLE",
  "item_id": "item-1"
}"#;
    let now = 1_800_000_000;

    plaid
        .verify_webhook(&jwt("ES256", now - 10, body), body, now)
        .await
        .unwrap();

    let tampered = br#"{"webhook_type": "TRANSACTIONS", "item_id": "item-2"}"#;
    assert!(matches!(
        plaid
            .verify_webhook(&jwt("ES256", now, body), tampered, now)
            .await,
        Err(WebhookError::Body)
    ));
    assert!(matches!(
        plaid
            .verify_webhook(&jwt("ES256", now - 301, body), body, now)
            .await,
        Err(WebhookError::Stale)
    ));
    assert!(matches!(
        plaid
            .verify_webhook(&jwt("HS256", now, body), body, now)
            .await,
        Err(WebhookError::Algorithm)
    ));
    let mut forged = jwt("ES256", now, body);
    forged.pop();
    forged.push(if forged.ends_with('A') { 'B' } else { 'A' });
    assert!(matches!(
        plaid.verify_webhook(&forged, body, now).await,
        Err(WebhookError::Signature | WebhookError::Malformed)
    ));
    assert!(matches!(
        plaid.verify_webhook("not-a-jwt", body, now).await,
        Err(WebhookError::Malformed)
    ));
}

#[tokio::test]
async fn rejects_expired_keys() {
    let server = MockServer::start().await;
    Mock::given(path("/webhook_verification_key/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwk_response(true)))
        .mount(&server)
        .await;
    let body = b"{}";
    let now = 1_800_000_000;
    let result = client(&server)
        .await
        .verify_webhook(&jwt("ES256", now, body), body, now)
        .await;
    assert!(matches!(result, Err(WebhookError::Key)), "{result:?}");
}
