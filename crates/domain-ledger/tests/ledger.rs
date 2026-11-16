//! Ledger tests against a real PostgreSQL 18.
//!
//! The service connects as `savespace_runtime`, so RLS applies. Run with:
//!   cargo nextest run --workspace --run-ignored only

#![expect(clippy::unwrap_used, reason = "tests")]

use domain_ledger::{
    CategoryFilter, LedgerError, LedgerService, NewSplit, TransactionFilter, TransactionPatch,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use uuid::Uuid;

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

#[derive(Clone, Copy)]
struct Home {
    household: Uuid,
    connection: Uuid,
    account: Uuid,
}

async fn household(admin: &PgPool) -> Home {
    let household = Uuid::now_v7();
    sqlx::query("INSERT INTO app.households (id) VALUES ($1)")
        .bind(household)
        .execute(admin)
        .await
        .unwrap();
    let connection: Uuid = sqlx::query_scalar(
        "INSERT INTO app.connections (household_id, provider, provider_item_id, \
           access_token_enc) VALUES ($1, 'plaid', $2, '\\x00') RETURNING id",
    )
    .bind(household)
    .bind(household.to_string())
    .fetch_one(admin)
    .await
    .unwrap();
    let account = add_account(admin, household, connection, "a1").await;
    Home {
        household,
        connection,
        account,
    }
}

async fn add_account(admin: &PgPool, household: Uuid, connection: Uuid, id: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO app.accounts (household_id, connection_id, provider_account_id, name, \
           type, currency) VALUES ($1, $2, $3, 'Checking', 'depository', 'USD') RETURNING id",
    )
    .bind(household)
    .bind(connection)
    .bind(id)
    .fetch_one(admin)
    .await
    .unwrap()
}

/// Adds a transaction. `category` is a seeded key for the automatic category.
async fn add_txn(
    admin: &PgPool,
    home: Home,
    account: Uuid,
    (name, date, amount, pending): (&str, &str, i64, bool),
    category: Option<&str>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO app.transactions (household_id, connection_id, account_id, \
           provider_transaction_id, raw_version, amount_minor, currency, date, name, pending, \
           category_id, category_source) \
         VALUES ($1, $2, $3, $4, 1, $5, 'USD', $6::date, $7, $8, \
           (SELECT id FROM app.categories WHERE household_id = $1 AND key = $9), \
           CASE WHEN $9::text IS NULL THEN NULL ELSE 'provider' END) \
         RETURNING id",
    )
    .bind(home.household)
    .bind(home.connection)
    .bind(account)
    .bind(Uuid::now_v7().to_string())
    .bind(amount)
    .bind(date)
    .bind(name)
    .bind(pending)
    .bind(category)
    .fetch_one(admin)
    .await
    .unwrap()
}

async fn category(admin: &PgPool, household: Uuid, key: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM app.categories WHERE household_id = $1 AND key = $2")
        .bind(household)
        .bind(key)
        .fetch_one(admin)
        .await
        .unwrap()
}

fn names(list: &[domain_ledger::Transaction]) -> Vec<&str> {
    list.iter().map(|t| t.name.as_str()).collect()
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn lists_with_filters_search_and_pages(admin: PgPool) {
    let svc = LedgerService::new(runtime_pool(&admin).await);
    let h = household(&admin).await;
    let savings = add_account(&admin, h.household, h.connection, "a2").await;
    add_txn(
        &admin,
        h,
        h.account,
        ("Blue Bottle", "2026-11-03", -450, false),
        Some("dining"),
    )
    .await;
    add_txn(
        &admin,
        h,
        h.account,
        ("Safeway 50%_OFF", "2026-11-02", -8000, false),
        Some("groceries"),
    )
    .await;
    add_txn(
        &admin,
        h,
        savings,
        ("Interest", "2026-11-01", 12, false),
        None,
    )
    .await;
    let hidden = add_txn(
        &admin,
        h,
        h.account,
        ("Refund", "2026-10-30", 1000, false),
        None,
    )
    .await;
    svc.update_transaction(
        h.household,
        hidden,
        TransactionPatch {
            hidden: Some(true),
            ..TransactionPatch::default()
        },
    )
    .await
    .unwrap();

    let all = TransactionFilter::default();
    let list = svc
        .list_transactions(h.household, &all, 50, None)
        .await
        .unwrap();
    assert_eq!(names(&list), ["Blue Bottle", "Safeway 50%_OFF", "Interest"]);

    let with_hidden = TransactionFilter {
        include_hidden: true,
        ..TransactionFilter::default()
    };
    let list = svc
        .list_transactions(h.household, &with_hidden, 50, None)
        .await
        .unwrap();
    assert_eq!(list.len(), 4);
    assert!(list[3].hidden);

    let by_account = TransactionFilter {
        account_id: Some(savings),
        ..TransactionFilter::default()
    };
    let list = svc
        .list_transactions(h.household, &by_account, 50, None)
        .await
        .unwrap();
    assert_eq!(names(&list), ["Interest"]);

    let groceries = category(&admin, h.household, "groceries").await;
    let by_category = TransactionFilter {
        category: CategoryFilter::Id(groceries),
        ..TransactionFilter::default()
    };
    let list = svc
        .list_transactions(h.household, &by_category, 50, None)
        .await
        .unwrap();
    assert_eq!(names(&list), ["Safeway 50%_OFF"]);
    assert_eq!(list[0].category_source.as_deref(), Some("provider"));

    let none = TransactionFilter {
        category: CategoryFilter::Uncategorized,
        ..TransactionFilter::default()
    };
    let list = svc
        .list_transactions(h.household, &none, 50, None)
        .await
        .unwrap();
    assert_eq!(names(&list), ["Interest"]);

    // Search is not case-sensitive, and % and _ are plain text.
    for (text, expected) in [
        ("blue", vec!["Blue Bottle"]),
        ("50%_", vec!["Safeway 50%_OFF"]),
        ("%", vec!["Safeway 50%_OFF"]),
        ("zzz", vec![]),
    ] {
        let f = TransactionFilter {
            search: Some(text.into()),
            ..TransactionFilter::default()
        };
        let list = svc
            .list_transactions(h.household, &f, 50, None)
            .await
            .unwrap();
        assert_eq!(names(&list), expected, "search {text:?}");
    }

    let dates = TransactionFilter {
        from: Some("2026-11-02".parse().unwrap()),
        to: Some("2026-11-02".parse().unwrap()),
        ..TransactionFilter::default()
    };
    let list = svc
        .list_transactions(h.household, &dates, 50, None)
        .await
        .unwrap();
    assert_eq!(names(&list), ["Safeway 50%_OFF"]);

    // Pages: newest first, each page starts after the last ID.
    let first = svc
        .list_transactions(h.household, &all, 2, None)
        .await
        .unwrap();
    let second = svc
        .list_transactions(h.household, &all, 2, Some(first[1].id))
        .await
        .unwrap();
    assert_eq!(names(&first), ["Blue Bottle", "Safeway 50%_OFF"]);
    assert_eq!(names(&second), ["Interest"]);
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn person_edits_win_and_clear(admin: PgPool) {
    let svc = LedgerService::new(runtime_pool(&admin).await);
    let h = household(&admin).await;
    let id = add_txn(
        &admin,
        h,
        h.account,
        ("SQ *COFFEE", "2026-11-03", -450, false),
        Some("dining"),
    )
    .await;
    let groceries = category(&admin, h.household, "groceries").await;

    let t = svc
        .update_transaction(
            h.household,
            id,
            TransactionPatch {
                category_id: Some(Some(groceries)),
                merchant_name: Some(Some("  Corner Shop ".into())),
                note: Some(Some("milk".into())),
                hidden: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(t.category_id, Some(groceries));
    assert_eq!(t.category_source.as_deref(), Some("person"));
    assert_eq!(t.merchant_name.as_deref(), Some("Corner Shop"));
    assert_eq!(t.note.as_deref(), Some("milk"));

    // A patch changes only the fields it has.
    let t = svc
        .update_transaction(
            h.household,
            id,
            TransactionPatch {
                note: Some(None),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(t.note, None);
    assert_eq!(t.category_source.as_deref(), Some("person"));

    // Clearing every edit removes the overlay: the automatic category is back.
    let t = svc
        .update_transaction(
            h.household,
            id,
            TransactionPatch {
                category_id: Some(None),
                merchant_name: Some(Some("   ".into())),
                ..TransactionPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(t.category_source.as_deref(), Some("provider"));
    assert_eq!(t.merchant_name, None);
    let overlays: i64 = sqlx::query_scalar("SELECT count(*) FROM app.user_overrides")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(overlays, 0);

    let too_long = svc
        .update_transaction(
            h.household,
            id,
            TransactionPatch {
                note: Some(Some("x".repeat(501))),
                ..TransactionPatch::default()
            },
        )
        .await;
    assert!(matches!(
        too_long,
        Err(LedgerError::Invalid {
            code: "note_invalid",
            ..
        })
    ));
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn splits_add_up_and_filter_by_part(admin: PgPool) {
    let svc = LedgerService::new(runtime_pool(&admin).await);
    let h = household(&admin).await;
    let id = add_txn(
        &admin,
        h,
        h.account,
        ("TARGET", "2026-11-03", -10_000, false),
        Some("shopping"),
    )
    .await;
    let pending = add_txn(
        &admin,
        h,
        h.account,
        ("PENDING", "2026-11-04", -500, true),
        None,
    )
    .await;
    let groceries = category(&admin, h.household, "groceries").await;
    let home = category(&admin, h.household, "home").await;
    let part = |category_id, amount_minor| NewSplit {
        category_id,
        amount_minor,
    };

    let code = |r: Result<domain_ledger::Transaction, LedgerError>| match r {
        Err(LedgerError::Invalid { code, .. }) => code,
        other => panic!("expected invalid, got {other:?}"),
    };
    assert_eq!(
        code(
            svc.set_splits(h.household, id, &[part(groceries, -10_000)])
                .await
        ),
        "splits_count"
    );
    assert_eq!(
        code(
            svc.set_splits(
                h.household,
                id,
                &[part(groceries, -6000), part(home, -3000)]
            )
            .await
        ),
        "splits_total"
    );
    assert_eq!(
        code(
            svc.set_splits(
                h.household,
                id,
                &[part(groceries, -11_000), part(home, 1000)]
            )
            .await
        ),
        "split_amount_invalid"
    );
    assert_eq!(
        code(
            svc.set_splits(
                h.household,
                pending,
                &[part(groceries, -250), part(home, -250)]
            )
            .await
        ),
        "transaction_pending"
    );

    let t = svc
        .set_splits(
            h.household,
            id,
            &[part(groceries, -6000), part(home, -4000)],
        )
        .await
        .unwrap();
    assert_eq!(t.splits.len(), 2);
    assert_eq!(t.splits[0].amount_minor, -6000);
    assert_eq!(t.splits[1].category_id, home);

    // A split transaction lists under each part's category, not its own.
    let by = |c| TransactionFilter {
        category: CategoryFilter::Id(c),
        ..TransactionFilter::default()
    };
    let shopping = category(&admin, h.household, "shopping").await;
    assert_eq!(
        svc.list_transactions(h.household, &by(home), 50, None)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        svc.list_transactions(h.household, &by(shopping), 50, None)
            .await
            .unwrap()
            .is_empty()
    );

    // An empty list removes the split.
    let t = svc.set_splits(h.household, id, &[]).await.unwrap();
    assert_eq!(t.splits, []);
}

#[sqlx::test(migrator = "platform_db::MIGRATOR")]
#[ignore = "needs DATABASE_URL"]
async fn households_cannot_see_or_edit_each_others_transactions(admin: PgPool) {
    let svc = LedgerService::new(runtime_pool(&admin).await);
    let a = household(&admin).await;
    let b = household(&admin).await;
    let id = add_txn(
        &admin,
        a,
        a.account,
        ("RENT", "2026-11-01", -180_000, false),
        None,
    )
    .await;
    let other = add_txn(&admin, b, b.account, ("B", "2026-11-01", -100, false), None).await;
    let category_a = category(&admin, a.household, "housing").await;

    assert!(
        svc.list_transactions(b.household, &TransactionFilter::default(), 50, None)
            .await
            .unwrap()
            .iter()
            .all(|t| t.id != id)
    );
    assert!(matches!(
        svc.get_transaction(b.household, id).await,
        Err(LedgerError::NotFound)
    ));
    assert!(matches!(
        svc.update_transaction(
            b.household,
            id,
            TransactionPatch {
                hidden: Some(true),
                ..TransactionPatch::default()
            }
        )
        .await,
        Err(LedgerError::NotFound)
    ));
    assert!(matches!(
        svc.set_splits(b.household, id, &[]).await,
        Err(LedgerError::NotFound)
    ));

    // B cannot put A's category on B's own transaction.
    let patch = TransactionPatch {
        category_id: Some(Some(category_a)),
        ..TransactionPatch::default()
    };
    assert!(matches!(
        svc.update_transaction(b.household, other, patch).await,
        Err(LedgerError::Invalid {
            code: "category_invalid",
            ..
        })
    ));
    let parts = [
        NewSplit {
            category_id: category_a,
            amount_minor: -50,
        },
        NewSplit {
            category_id: category_a,
            amount_minor: -50,
        },
    ];
    assert!(matches!(
        svc.set_splits(b.household, other, &parts).await,
        Err(LedgerError::Invalid {
            code: "category_invalid",
            ..
        })
    ));
}
