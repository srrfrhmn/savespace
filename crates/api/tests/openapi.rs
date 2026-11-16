//! The committed `OpenAPI` document must match the code (design doc §11.1).
//!
//! To update it, run `pnpm gen:api`. It sets `SAVESPACE_UPDATE_OPENAPI=1`,
//! runs this test, then regenerates the web client types.

use std::path::PathBuf;

#[test]
fn openapi_document_is_current() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../clients/openapi.json");
    let generated = api::openapi()
        .to_pretty_json()
        .expect("the document serializes")
        + "\n";
    if std::env::var_os("SAVESPACE_UPDATE_OPENAPI").is_some() {
        std::fs::write(&path, &generated).expect("write clients/openapi.json");
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        committed == generated,
        "clients/openapi.json is out of date. Run: pnpm gen:api"
    );
}
