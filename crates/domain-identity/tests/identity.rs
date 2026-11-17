//! Identity and tenancy tests against a real PostgreSQL 18.
//!
//! Each test gets a new database with all migrations applied. The service
//! connects as `savespace_runtime`, like production, so row-level security
//! applies. Run with:
//!   cargo nextest run --workspace --run-ignored only

#![expect(clippy::unwrap_used, reason = "tests")]

use domain_identity::{Identity, IdentityConfig, IdentityError, RequestContext, token};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use uuid::Uuid;

/// A pool that connects as the runtime role (not the table owner).
async fn runtime_pool(admin: &PgPool) -> PgPool {
    PgPoolOptions::new()
        .max_connections(4)
        .after_connect(|conn, _| {
            Box::pin(async move {
                conn.execute("SET ROLE savespace_runtime").await?;
                Ok(())
            })
        })
        .connect_with((*admin.connect_options()).clone())
        .await
        .unwrap()
}

fn identity(pool: &PgPool, setup_code: Option<&str>) -> Identity {
    Identity::new(
        pool.clone(),
        &IdentityConfig {
            origin: "https://app.example.test".parse().unwrap(),
            setup_code: setup_code.map(str::to_owned),
        },
    )
    .unwrap()
}

/// Creates an owner with one known recovery code, as the admin role.
async fn seed_owner(admin: &PgPool, code: &str) -> (Uuid, Uuid) {
    let household = Uuid::now_v7();
    let user = Uuid::now_v7();
    let created: bool = sqlx::query_scalar("SELECT app.create_owner($1, $2, 'Sam')")
        .bind(user)
        .bind(household)
        .fetch_one(admin)
        .await
        .unwrap();
    assert!(created);
    let normalized = token::normalize_recovery_code(code).unwrap();
    sqlx::query(
        "INSERT INTO app.recovery_codes (household_id, user_id, code_hash) VALUES ($1, $2, $3)",
    )
    .bind(household)
    .bind(user)
    .bind(token::hash_recovery_code(&normalized))
    .execute(admin)
    .await
    .unwrap();
    (household, user)
}

/// Adds a second household directly, as the admin role. `create_owner` runs
/// only once, so this inserts the rows itself.
async fn seed_other_household(admin: &PgPool, code: &str) -> (Uuid, Uuid) {
    let household = Uuid::now_v7();
    let user = Uuid::now_v7();
    sqlx::query("INSERT INTO app.households (id) VALUES ($1)")
        .bind(household)
        .execute(admin)
        .await
        .unwrap();
    sqlx::query("INSERT INTO app.users (id, display_name) VALUES ($1, 'Alex')")
        .bind(user)
        .execute(admin)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO app.household_members (household_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(household)
    .bind(user)
    .execute(admin)
    .await
    .unwrap();
    let normalized = token::normalize_recovery_code(code).unwrap();
    sqlx::query(
        "INSERT INTO app.recovery_codes (household_id, user_id, code_hash) VALUES ($1, $2, $3)",
    )
    .bind(household)
    .bind(user)
    .bind(token::hash_recovery_code(&normalized))
    .execute(admin)
    .await
    .unwrap();
    (household, user)
}

const CODE_A: &str = "AAAA-BBBB-CCCC";
const CODE_B: &str = "DDDD-EEEE-FFFF";

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn setup_needs_a_code_and_runs_once(admin: PgPool) {
    let pool = runtime_pool(&admin).await;

    let closed = identity(&pool, None);
    assert!(!closed.setup_status().await.unwrap().setup_open);
    assert!(matches!(
        closed.start_setup("anything", "Sam").await,
        Err(IdentityError::SetupUnavailable)
    ));

    let open = identity(&pool, Some("right-code"));
    assert!(open.setup_status().await.unwrap().setup_open);
    assert!(matches!(
        open.start_setup("wrong-code", "Sam").await,
        Err(IdentityError::InvalidSetupCode)
    ));
    let challenge = open.start_setup(" right-code ", "Sam").await.unwrap();
    assert_eq!(
        challenge.options["publicKey"]["authenticatorSelection"]["residentKey"],
        "required"
    );
    assert_eq!(
        challenge.options["publicKey"]["rp"]["id"],
        "app.example.test"
    );

    seed_owner(&admin, CODE_A).await;
    assert!(!open.setup_status().await.unwrap().setup_open);
    assert!(matches!(
        open.start_setup("right-code", "Sam").await,
        Err(IdentityError::AlreadySetUp)
    ));
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn recovery_code_works_once_and_sessions_end(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    let (household, user) = seed_owner(&admin, CODE_A).await;
    let id = identity(&pool, None);
    let ctx = RequestContext::default();

    let typed = CODE_A.to_lowercase().replace('-', " ");
    let session = id.login_with_recovery_code(&typed, &ctx).await.unwrap();
    assert_eq!(session.info.household_id, household);
    assert_eq!(session.info.user_id, user);
    assert!(session.info.step_up_valid(jiff::Timestamp::now()));

    assert!(matches!(
        id.login_with_recovery_code(CODE_A, &ctx).await,
        Err(IdentityError::InvalidRecoveryCode)
    ));
    assert!(matches!(
        id.login_with_recovery_code("not a code", &ctx).await,
        Err(IdentityError::InvalidRecoveryCode)
    ));

    let found = id.authenticate(&session.token).await.unwrap().unwrap();
    assert_eq!(found.session_id, session.info.session_id);
    assert!(id.authenticate("unknown").await.unwrap().is_none());

    let me = id.me(&found).await.unwrap();
    assert_eq!(me.display_name, "Sam");
    assert_eq!(me.recovery_codes_left, 0);

    let codes = id.regenerate_recovery_codes(&found, &ctx).await.unwrap();
    assert_eq!(codes.len(), token::RECOVERY_CODE_COUNT);
    assert_eq!(id.me(&found).await.unwrap().recovery_codes_left, 10);

    id.logout(&found, &ctx).await.unwrap();
    assert!(id.authenticate(&session.token).await.unwrap().is_none());

    // The audit log has the events, and the runtime role cannot change it.
    let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM app.audit_log ORDER BY id")
        .fetch_all(&admin)
        .await
        .unwrap();
    assert!(actions.contains(&"login.recovery_code".to_owned()));
    assert!(actions.contains(&"login.recovery_code_invalid".to_owned()));
    assert!(actions.contains(&"logout".to_owned()));
    let update = sqlx::query("UPDATE app.audit_log SET action = 'x'")
        .execute(&pool)
        .await;
    assert!(
        update.is_err(),
        "runtime role must not update the audit log"
    );
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn expired_sessions_do_not_authenticate(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    seed_owner(&admin, CODE_A).await;
    let id = identity(&pool, None);
    let session = id
        .login_with_recovery_code(CODE_A, &RequestContext::default())
        .await
        .unwrap();

    sqlx::query("UPDATE app.sessions SET last_seen_at = now() - interval '31 days'")
        .execute(&admin)
        .await
        .unwrap();
    assert!(
        id.authenticate(&session.token).await.unwrap().is_none(),
        "idle limit"
    );

    sqlx::query(
        "UPDATE app.sessions SET last_seen_at = now(), expires_at = now() - interval '1 second'",
    )
    .execute(&admin)
    .await
    .unwrap();
    assert!(
        id.authenticate(&session.token).await.unwrap().is_none(),
        "absolute limit"
    );

    let deleted = domain_identity::delete_expired(&pool).await.unwrap();
    assert_eq!(deleted, 1);
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn row_level_security_isolates_households(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    let (household_a, _) = seed_owner(&admin, CODE_A).await;
    let (household_b, user_b) = seed_other_household(&admin, CODE_B).await;
    let id = identity(&pool, None);
    let ctx = RequestContext::default();
    let a = id.login_with_recovery_code(CODE_A, &ctx).await.unwrap();
    id.login_with_recovery_code(CODE_B, &ctx).await.unwrap();

    // Without a household, the runtime role sees no tenant rows.
    let counts: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM app.households), (SELECT count(*) FROM app.users),          (SELECT count(*) FROM app.household_members), (SELECT count(*) FROM app.sessions),          (SELECT count(*) FROM app.recovery_codes)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (0, 0, 0, 0, 0), "no rows without a household");

    // In household A, only A's rows are visible.
    let mut tx = platform_db::tenant_tx(&pool, household_a).await.unwrap();
    let households: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM app.households")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert_eq!(households, vec![household_a]);
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM app.users")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(users, 1);
    let sessions: Vec<Uuid> = sqlx::query_scalar("SELECT household_id FROM app.sessions")
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert_eq!(sessions, vec![household_a]);

    // Writes into household B fail or change nothing.
    let insert = sqlx::query(
        "INSERT INTO app.recovery_codes (household_id, user_id, code_hash) \
         VALUES ($1, $2, decode(repeat('ab', 32), 'hex'))",
    )
    .bind(household_b)
    .bind(user_b)
    .execute(&mut *tx)
    .await;
    assert!(insert.is_err(), "insert into another household");
    tx.rollback().await.unwrap();

    let mut tx = platform_db::tenant_tx(&pool, household_a).await.unwrap();
    let updated = sqlx::query("UPDATE app.users SET display_name = 'x' WHERE id = $1")
        .bind(user_b)
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
    let deleted = sqlx::query("DELETE FROM app.sessions WHERE household_id = $1")
        .bind(household_b)
        .execute(&mut *tx)
        .await
        .unwrap()
        .rows_affected();
    tx.commit().await.unwrap();
    assert_eq!((updated, deleted), (0, 0));

    // The setting ends with the transaction.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM app.sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);

    // A session lookup returns only that session's household.
    let found = id.authenticate(&a.token).await.unwrap().unwrap();
    assert_eq!(found.household_id, household_a);
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn passkey_delete_rules(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    let (household, user) = seed_owner(&admin, CODE_A).await;
    let id = identity(&pool, None);
    let ctx = RequestContext::default();
    let session = id.login_with_recovery_code(CODE_A, &ctx).await.unwrap();

    // Two stored passkeys. The JSON is not read by delete.
    let mut ids = Vec::new();
    for n in 0..2_u8 {
        let pk: Uuid = sqlx::query_scalar(
            "INSERT INTO app.passkeys (household_id, user_id, credential_id, passkey, name) \
             VALUES ($1, $2, $3, '{}', 'key') RETURNING id",
        )
        .bind(household)
        .bind(user)
        .bind(vec![n; 16])
        .fetch_one(&admin)
        .await
        .unwrap();
        ids.push(pk);
    }

    assert!(matches!(
        id.delete_passkey(&session.info, Uuid::now_v7(), &ctx).await,
        Err(IdentityError::NotFound)
    ));
    id.delete_passkey(&session.info, ids[0], &ctx)
        .await
        .unwrap();
    assert!(matches!(
        id.delete_passkey(&session.info, ids[1], &ctx).await,
        Err(IdentityError::LastPasskey)
    ));
}

/// Every table with household data is in the export or listed as left out
/// (design doc §12.5). A new table must choose.
#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn export_covers_every_household_table(admin: PgPool) {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.columns \
         WHERE table_schema = 'app' AND column_name = 'household_id' ORDER BY 1",
    )
    .fetch_all(&admin)
    .await
    .unwrap();
    assert!(tables.iter().any(|t| t == "insight_reviews"));
    let missing: Vec<&String> = tables
        .iter()
        .filter(|t| {
            !domain_identity::export::covers(t)
                && !domain_identity::export::NOT_EXPORTED.contains(&t.as_str())
        })
        .collect();
    assert!(missing.is_empty(), "not in the export: {missing:?}");
}
