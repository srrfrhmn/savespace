//! Sync tests against a real PostgreSQL 18 with a scripted aggregator.
//!
//! The service connects as `savespace_runtime`, so RLS applies. Run with:
//!   cargo nextest run --workspace --run-ignored only

#![expect(clippy::unwrap_used, reason = "tests")]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine;
use domain_sync::{
    Aggregator, AggregatorError, ConnectionStatus, LinkToken, LinkTokenRequest, NewItem,
    ProviderAccount, ProviderBalances, ProviderTransaction, SyncError, SyncPage, SyncService,
    WebhookAction,
};
use platform_crypto::{Kek, Keyring};
use savespace_core::{Currency, Money};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use uuid::Uuid;

type Script = HashMap<Option<String>, VecDeque<Result<SyncPage, AggregatorError>>>;

/// Answers `sync_page` from a script keyed by cursor.
#[derive(Default)]
struct Fake {
    script: Mutex<Script>,
    removed: Mutex<Vec<String>>,
    /// If set, the next fetch moves the stored cursor first, like a sync
    /// that runs at the same time.
    race: Mutex<Option<(PgPool, String)>>,
}

impl Fake {
    fn on(&self, cursor: Option<&str>, page: Result<SyncPage, AggregatorError>) {
        self.script
            .lock()
            .unwrap()
            .entry(cursor.map(str::to_owned))
            .or_default()
            .push_back(page);
    }
}

#[async_trait]
impl Aggregator for Fake {
    fn provider(&self) -> &'static str {
        "plaid"
    }

    async fn create_link_token(
        &self,
        request: &LinkTokenRequest,
    ) -> Result<LinkToken, AggregatorError> {
        Ok(LinkToken {
            link_token: format!(
                "link-{}-{}",
                request.client_user_id,
                request.update_access_token.as_deref().unwrap_or("new")
            ),
            expiration: None,
        })
    }

    async fn exchange_public_token(&self, public_token: &str) -> Result<NewItem, AggregatorError> {
        Ok(NewItem {
            item_id: format!("item-{public_token}"),
            access_token: format!("access-{public_token}"),
        })
    }

    async fn sync_page(
        &self,
        _access_token: &str,
        cursor: Option<&str>,
    ) -> Result<SyncPage, AggregatorError> {
        let race = self.race.lock().unwrap().take();
        if let Some((admin, moved_to)) = race {
            sqlx::query("UPDATE app.connections SET sync_cursor = $1")
                .bind(moved_to)
                .execute(&admin)
                .await
                .unwrap();
        }
        self.script
            .lock()
            .unwrap()
            .get_mut(&cursor.map(str::to_owned))
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| panic!("no scripted page for cursor {cursor:?}"))
    }

    async fn remove_item(&self, access_token: &str) -> Result<(), AggregatorError> {
        self.removed.lock().unwrap().push(access_token.to_owned());
        Ok(())
    }
}

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

async fn household(admin: &PgPool) -> (Uuid, Uuid) {
    let household = Uuid::now_v7();
    let user = Uuid::now_v7();
    sqlx::query("INSERT INTO app.households (id) VALUES ($1)")
        .bind(household)
        .execute(admin)
        .await
        .unwrap();
    sqlx::query("INSERT INTO app.users (id, display_name) VALUES ($1, 'Sam')")
        .bind(user)
        .execute(admin)
        .await
        .unwrap();
    (household, user)
}

fn service(pool: &PgPool, fake: Arc<Fake>) -> SyncService {
    let kek =
        Kek::from_base64(&base64::engine::general_purpose::STANDARD.encode([9_u8; 32])).unwrap();
    SyncService::new(pool.clone(), Keyring::new(kek), fake)
}

fn account(id: &str, current: i64) -> ProviderAccount {
    ProviderAccount {
        provider_account_id: id.to_owned(),
        name: format!("Account {id}"),
        official_name: None,
        mask: Some("0000".into()),
        kind: "depository".into(),
        subtype: Some("checking".into()),
        currency: Currency::Usd,
        balances: ProviderBalances {
            current: Some(current),
            available: Some(current),
            limit: None,
        },
    }
}

fn txn(id: &str, minor: i64, pending: bool) -> ProviderTransaction {
    ProviderTransaction {
        provider_transaction_id: id.to_owned(),
        provider_account_id: "acc-1".into(),
        amount: Money::from_minor(minor, Currency::Usd),
        date: jiff::civil::date(2026, 11, 10),
        authorized_date: None,
        name: format!("Txn {id}"),
        merchant_name: Some("Corner Cafe".into()),
        pending,
        pending_transaction_id: None,
        category_primary: Some("FOOD_AND_DRINK".into()),
        category_detailed: None,
        raw: serde_json::json!({ "transaction_id": id, "amount": -minor }),
    }
}

#[expect(clippy::unnecessary_wraps, reason = "the script stores results")]
fn page(
    added: Vec<ProviderTransaction>,
    modified: Vec<ProviderTransaction>,
    removed: &[&str],
    next: &str,
    has_more: bool,
) -> Result<SyncPage, AggregatorError> {
    Ok(SyncPage {
        accounts: vec![account("acc-1", 100_000)],
        added,
        modified,
        removed: removed.iter().map(|s| (*s).to_owned()).collect(),
        next_cursor: next.to_owned(),
        has_more,
    })
}

async fn count(admin: &PgPool, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(admin).await.unwrap()
}

/// The current transactions as (name, amount).
async fn current(admin: &PgPool) -> Vec<(String, i64)> {
    sqlx::query_as("SELECT name, amount_minor FROM app.transactions ORDER BY name")
        .fetch_all(admin)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn connect_then_sync_pages_and_changes(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    let (household, user) = household(&admin).await;
    let fake = Arc::new(Fake::default());
    let svc = service(&pool, fake.clone());

    let conn = svc
        .create_connection(household, user, "pub1", Some(("ins_1", "First Bank")), None)
        .await
        .unwrap();
    assert_eq!(conn.status, "syncing");
    assert_eq!(conn.institution_name.as_deref(), Some("First Bank"));

    // The access token is encrypted, and the first sync is queued.
    let sealed: Vec<u8> = sqlx::query_scalar("SELECT access_token_enc FROM app.connections")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains("access-pub1"));
    assert_eq!(
        count(
            &admin,
            "SELECT count(*) FROM app.jobs WHERE kind = 'sync.connection'"
        )
        .await,
        1
    );

    // First sync: two pages.
    fake.on(
        None,
        page(
            vec![txn("t1", -1250, false), txn("t2", -400, true)],
            vec![],
            &[],
            "c1",
            true,
        ),
    );
    fake.on(
        Some("c1"),
        page(vec![txn("t3", 250_000, false)], vec![], &[], "c2", false),
    );
    let outcome = svc.sync_connection(household, conn.id).await.unwrap();
    assert_eq!(
        (outcome.added, outcome.modified, outcome.removed),
        (3, 0, 0)
    );

    let listed = current(&admin).await;
    assert_eq!(listed.len(), 3);
    let accounts = svc.list_accounts(household).await.unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].current_minor, Some(100_000));
    let connections = svc.list_connections(household).await.unwrap();
    assert_eq!(connections[0].status, "ok");
    assert!(connections[0].last_synced_at.is_some());

    // The person edits the pending t2. Enrichment has run on t1.
    sqlx::query(
        "INSERT INTO app.user_overrides (transaction_id, household_id, note) \
         SELECT id, household_id, 'lunch' FROM app.transactions \
         WHERE provider_transaction_id = 't2'",
    )
    .execute(&admin)
    .await
    .unwrap();
    sqlx::query("UPDATE app.transactions SET enriched_version = raw_version")
        .execute(&admin)
        .await
        .unwrap();

    // Second sync: t1 changes, the pending t2 posts as t4, t3 is removed.
    let mut t1 = txn("t1", -1300, false);
    t1.name = "Txn t1 corrected".into();
    let mut t4 = txn("t4", -400, false);
    t4.pending_transaction_id = Some("t2".into());
    fake.on(
        Some("c2"),
        page(vec![t4], vec![t1], &["t2", "t3"], "c3", false),
    );
    let outcome = svc.sync_connection(household, conn.id).await.unwrap();
    assert_eq!(
        (outcome.added, outcome.modified, outcome.removed),
        (1, 1, 2)
    );

    let ids = current(&admin).await;
    assert_eq!(
        ids,
        [
            ("Txn t1 corrected".to_owned(), -1300),
            ("Txn t4".to_owned(), -400)
        ]
    );
    let pending_link: Option<String> = sqlx::query_scalar(
        "SELECT pending_provider_transaction_id FROM app.transactions \
         WHERE provider_transaction_id = 't4'",
    )
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(pending_link.as_deref(), Some("t2"));
    // The edit moved to the posted transaction. Changed rows enrich again.
    let moved: Option<String> = sqlx::query_scalar(
        "SELECT t.provider_transaction_id FROM app.user_overrides o \
         JOIN app.transactions t ON t.id = o.transaction_id WHERE o.note = 'lunch'",
    )
    .fetch_optional(&admin)
    .await
    .unwrap();
    assert_eq!(moved.as_deref(), Some("t4"));
    let unenriched: Vec<String> = sqlx::query_scalar(
        "SELECT provider_transaction_id FROM app.transactions \
         WHERE enriched_version IS NULL ORDER BY 1",
    )
    .fetch_all(&admin)
    .await
    .unwrap();
    assert_eq!(unenriched, ["t1", "t4"]);

    // Raw data is append-only: t1 has two versions; removed rows are marked.
    assert_eq!(
        count(
            &admin,
            "SELECT count(*) FROM app.transactions_raw WHERE provider_transaction_id = 't1'"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &admin,
            "SELECT count(*) FROM app.transactions_raw WHERE removed_at IS NOT NULL"
        )
        .await,
        2
    );
    let raw_version: i32 = sqlx::query_scalar(
        "SELECT raw_version FROM app.transactions WHERE provider_transaction_id = 't1'",
    )
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(raw_version, 2);

    // Two balance snapshots, two outbox events, the last cursor.
    assert_eq!(count(&admin, "SELECT count(*) FROM app.balances").await, 2);
    assert_eq!(
        count(
            &admin,
            "SELECT count(*) FROM app.outbox WHERE topic = 'transactions.changed'"
        )
        .await,
        2
    );
    let cursor: String = sqlx::query_scalar("SELECT sync_cursor FROM app.connections")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(cursor, "c3");

    // Delete: removed at the provider, then all rows are gone.
    svc.delete_connection(household, user, conn.id, None)
        .await
        .unwrap();
    assert_eq!(fake.removed.lock().unwrap().as_slice(), ["access-pub1"]);
    assert_eq!(
        count(&admin, "SELECT count(*) FROM app.transactions").await,
        0
    );
    assert_eq!(count(&admin, "SELECT count(*) FROM app.accounts").await, 0);
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn mutation_during_pagination_restarts_from_the_first_cursor(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    let (household, user) = household(&admin).await;
    let fake = Arc::new(Fake::default());
    let svc = service(&pool, fake.clone());
    let conn = svc
        .create_connection(household, user, "pub2", None, None)
        .await
        .unwrap();

    fake.on(
        None,
        page(vec![txn("a", -100, false)], vec![], &[], "p1", true),
    );
    fake.on(Some("p1"), Err(AggregatorError::MutationDuringPagination));
    // The restart from the first cursor gets fresh pages.
    fake.on(
        None,
        page(
            vec![txn("a", -100, false), txn("b", -200, false)],
            vec![],
            &[],
            "p1b",
            true,
        ),
    );
    fake.on(
        Some("p1b"),
        page(vec![txn("c", -300, false)], vec![], &[], "done", false),
    );

    let outcome = svc.sync_connection(household, conn.id).await.unwrap();
    assert_eq!(outcome.added, 3);
    assert_eq!(
        count(&admin, "SELECT count(*) FROM app.transactions").await,
        3
    );
    assert_eq!(
        count(&admin, "SELECT count(*) FROM app.transactions_raw").await,
        3
    );
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn login_required_sets_status_and_webhooks_queue_work(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    let (household, user) = household(&admin).await;
    let fake = Arc::new(Fake::default());
    let svc = service(&pool, fake.clone());
    let conn = svc
        .create_connection(household, user, "pub3", None, None)
        .await
        .unwrap();

    fake.on(
        None,
        Err(AggregatorError::LoginRequired {
            code: "ITEM_LOGIN_REQUIRED".into(),
        }),
    );
    let result = svc.sync_connection(household, conn.id).await;
    assert!(
        matches!(result, Err(SyncError::NeedsAttention)),
        "{result:?}"
    );
    let status: (String, Option<String>) =
        sqlx::query_as("SELECT status, error_code FROM app.connections")
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(
        status,
        ("login_required".into(), Some("ITEM_LOGIN_REQUIRED".into()))
    );

    // The update-mode link token uses the stored access token.
    let link = svc
        .create_link_token(household, "usr_x", None, Some(conn.id))
        .await
        .unwrap();
    assert_eq!(link.link_token, "link-usr_x-access-pub3");

    // A webhook for this item: stored, then applied.
    sqlx::query("UPDATE app.jobs SET status = 'succeeded'")
        .execute(&admin)
        .await
        .unwrap();
    let body = serde_json::json!({ "item_id": "item-pub3" });
    let event = svc
        .store_webhook(
            Some("item-pub3"),
            "TRANSACTIONS",
            "SYNC_UPDATES_AVAILABLE",
            &body,
        )
        .await
        .unwrap();
    assert_eq!(
        count(
            &admin,
            "SELECT count(*) FROM app.jobs WHERE kind = 'sync.webhook' AND status = 'queued'"
        )
        .await,
        1
    );
    svc.apply_webhook(event, Some("item-pub3"), &WebhookAction::Sync)
        .await
        .unwrap();
    assert_eq!(
        count(
            &admin,
            "SELECT count(*) FROM app.jobs WHERE kind = 'sync.connection' AND status = 'queued'"
        )
        .await,
        1
    );
    let event2 = svc
        .store_webhook(Some("item-pub3"), "ITEM", "PENDING_EXPIRATION", &body)
        .await
        .unwrap();
    svc.apply_webhook(
        event2,
        Some("item-pub3"),
        &WebhookAction::SetStatus {
            status: ConnectionStatus::PendingExpiration,
            error_code: None,
        },
    )
    .await
    .unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM app.connections")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(status, "pending_expiration");
    assert_eq!(
        count(
            &admin,
            "SELECT count(*) FROM app.webhook_events WHERE processed_at IS NOT NULL"
        )
        .await,
        2
    );

    // Unknown items are ignored.
    let event3 = svc
        .store_webhook(
            Some("item-unknown"),
            "TRANSACTIONS",
            "SYNC_UPDATES_AVAILABLE",
            &body,
        )
        .await
        .unwrap();
    svc.apply_webhook(event3, Some("item-unknown"), &WebhookAction::Sync)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn households_cannot_see_each_others_connections(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    let (a, user_a) = household(&admin).await;
    let (b, _) = household(&admin).await;
    let fake = Arc::new(Fake::default());
    let svc = service(&pool, fake.clone());
    let conn = svc
        .create_connection(a, user_a, "pubA", None, None)
        .await
        .unwrap();
    fake.on(
        None,
        page(vec![txn("x", -100, false)], vec![], &[], "c", false),
    );
    svc.sync_connection(a, conn.id).await.unwrap();

    assert!(svc.list_connections(b).await.unwrap().is_empty());
    assert!(svc.list_accounts(b).await.unwrap().is_empty());
    assert!(matches!(
        svc.sync_connection(b, conn.id).await,
        Err(SyncError::NotFound)
    ));
    assert!(matches!(
        svc.request_sync(b, conn.id).await,
        Err(SyncError::NotFound)
    ));
    assert!(matches!(
        svc.delete_connection(b, user_a, conn.id, None).await,
        Err(SyncError::NotFound)
    ));
    assert_eq!(
        count(&admin, "SELECT count(*) FROM app.connections").await,
        1
    );

    // The poll finds the connection only when it is old enough.
    assert_eq!(
        svc.enqueue_due_polls(std::time::Duration::from_hours(6))
            .await
            .unwrap(),
        0
    );
    sqlx::query("UPDATE app.connections SET last_synced_at = now() - interval '7 hours'")
        .execute(&admin)
        .await
        .unwrap();
    assert_eq!(
        svc.enqueue_due_polls(std::time::Duration::from_hours(6))
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn a_concurrent_sync_does_not_overwrite_the_cursor(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    let (household, user) = household(&admin).await;
    let fake = Arc::new(Fake::default());
    let svc = service(&pool, fake.clone());
    let conn = svc
        .create_connection(household, user, "pub4", None, None)
        .await
        .unwrap();

    // Another sync moves the cursor while this one fetches.
    *fake.race.lock().unwrap() = Some((admin.clone(), "theirs".into()));
    fake.on(
        None,
        page(vec![txn("r", -1, false)], vec![], &[], "mine", false),
    );
    let result = svc.sync_connection(household, conn.id).await;
    assert!(matches!(result, Err(SyncError::Concurrent)), "{result:?}");

    // Nothing from the losing sync was written.
    let cursor: String = sqlx::query_scalar("SELECT sync_cursor FROM app.connections")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(cursor, "theirs");
    assert_eq!(
        count(&admin, "SELECT count(*) FROM app.transactions").await,
        0
    );
}

/// The same bank twice: the new item is removed at the provider and nothing
/// is stored, so transactions do not count twice.
#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn same_institution_twice_is_refused(admin: PgPool) {
    let pool = runtime_pool(&admin).await;
    let (household, user) = household(&admin).await;
    let fake = Arc::new(Fake::default());
    let svc = service(&pool, fake.clone());

    svc.create_connection(household, user, "pub1", Some(("ins_1", "First Bank")), None)
        .await
        .unwrap();
    let err = svc
        .create_connection(household, user, "pub2", Some(("ins_1", "First Bank")), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, domain_sync::SyncError::AlreadyConnected(name) if name == "First Bank"),
        "{err:?}"
    );
    assert_eq!(*fake.removed.lock().unwrap(), ["access-pub2"]);
    assert_eq!(
        count(&admin, "SELECT count(*) FROM app.connections").await,
        1
    );

    // Another bank, or no institution in the Link metadata, still connects.
    svc.create_connection(
        household,
        user,
        "pub3",
        Some(("ins_2", "Second Bank")),
        None,
    )
    .await
    .unwrap();
    svc.create_connection(household, user, "pub4", None, None)
        .await
        .unwrap();
    assert_eq!(
        count(&admin, "SELECT count(*) FROM app.connections").await,
        3
    );
}
