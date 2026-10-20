//! Migrations are append-only: a library created by any earlier build must open with this one.

use std::borrow::Cow;

use sha2::{Digest, Sha384};
use shorts_store::{Store, StoreError};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection};

/// SHA-384 of every migration as shipped, with LF line endings (`.gitattributes` keeps them LF).
/// A migration that has shipped must never change, because SQLx then refuses to open libraries
/// that already applied it. To change the schema, add a new migration and list it here.
const SHIPPED: &[(i64, &str)] = &[
    (
        1,
        "f1b7b70c6beee0f8738f5a44d48a96767bb961a16a828bece5274c0867ceb13dadc459ca6a9770f1f9f8d2bb0f10caf9",
    ),
    (
        2,
        "90a0d144916cf1738905c3978b07e17ab4b0066ee9b5a1bd41de2322f33df3501196b32f6c36bfadc442c19c430916d5",
    ),
    (
        3,
        "e40afcd931789acb745547f492a239e456c0d6f5f952c050cbf45b2ee874cc15b41aa537ae1d6317129532253e056909",
    ),
    (
        4,
        "417478e22ab0adeab79ad5be244495acedc66a428be0737d364a55c1cb9044f44df5750e4524586d3f47709ea2a9610b",
    ),
    (
        5,
        "9a8c62f478a9d7c75f7c1fc1bccaa1a0217212f470a06779bfddeb6a8f7cb5d60cad25a87a0c99308b893a926ef16498",
    ),
    (
        6,
        "07e65f58d9119922e9ce71f052500d01b082cdb39a9d78f474b48eab0322481fb6f62363e0e8ba8df3d3757bd923472f",
    ),
];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn lf_checksum(sql: &str) -> String {
    hex(&Sha384::digest(sql.replace("\r\n", "\n").as_bytes()))
}

#[test]
fn shipped_migrations_never_change() {
    let migrator = sqlx::migrate!("./migrations");
    for m in migrator.iter() {
        let Some((_, expected)) = SHIPPED.iter().find(|(v, _)| *v == m.version) else {
            panic!(
                "migration {} ({}) isn't listed in SHIPPED; add it with checksum {}",
                m.version,
                m.description,
                lf_checksum(&m.sql)
            );
        };
        assert_eq!(
            lf_checksum(&m.sql),
            *expected,
            "migration {} ({}) was edited after it shipped. Restore it and put the change in a \
             new migration.",
            m.version,
            m.description
        );
    }
}

/// Applies migrations up to `last` only, as an older build would have.
async fn old_library(path: &std::path::Path, last: i64) {
    let mut conn = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .connect()
        .await
        .unwrap();
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.migrations = Cow::Owned(
        migrator
            .migrations
            .iter()
            .filter(|m| m.version <= last)
            .cloned()
            .collect(),
    );
    migrator.run(&mut conn).await.unwrap();
    conn.close().await.unwrap();
}

#[tokio::test]
async fn a_library_from_before_rule_start_dates_upgrades() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.db");
    old_library(&path, 2).await;
    {
        let mut conn = SqliteConnectOptions::new()
            .filename(&path)
            .connect()
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO collections (id, name, created_at, updated_at) \
             VALUES ('c1', 'Old', '2026-10-17T19:00:00Z', '2026-10-17T19:00:00Z')",
        )
        .execute(&mut conn)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO collection_rules (id, collection_id, spec, created_at, updated_at) \
             VALUES ('r1', 'c1', '{}', '2026-10-17T19:30:00Z', '2026-10-17T19:30:00Z')",
        )
        .execute(&mut conn)
        .await
        .unwrap();
        conn.close().await.unwrap();
    }

    let store = Store::open(&path).await.unwrap();
    let effective: String =
        sqlx::query_scalar("SELECT effective_from FROM collection_rules WHERE id = 'r1'")
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(
        effective, "2026-10-17T19:30:00Z",
        "old rules count from creation"
    );
}

#[tokio::test]
async fn line_ending_changes_are_accepted_but_edits_are_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.db");
    let store = Store::open(&path).await.unwrap();
    let migrator = sqlx::migrate!("./migrations");
    let first = migrator.iter().next().unwrap();
    let crlf = first.sql.replace("\r\n", "\n").replace('\n', "\r\n");
    // As if the library had been created from a checkout with CRLF line endings.
    sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
        .bind(Sha384::digest(crlf.as_bytes()).to_vec())
        .bind(first.version)
        .execute(store.pool())
        .await
        .unwrap();
    store.close().await;

    let store = Store::open(&path).await.unwrap();
    let recorded: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = ?")
            .bind(first.version)
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(recorded, first.checksum.to_vec());

    // A real edit is still refused.
    sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
        .bind(vec![0u8; 48])
        .bind(first.version)
        .execute(store.pool())
        .await
        .unwrap();
    store.close().await;
    assert!(matches!(
        Store::open(&path).await,
        Err(StoreError::Migration(_))
    ));
}
