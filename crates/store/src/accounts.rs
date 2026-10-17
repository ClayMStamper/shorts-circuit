use shorts_domain::{AccountId, ConnectedAccount, Platform};
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteRow;

use crate::codec::{RowExt, json, ts};
use crate::{StoreError, StoreResult};

pub struct Accounts<'a> {
    pool: &'a SqlitePool,
}

const COLUMNS: &str =
    "id, platform, display_name, capabilities, credential_ref, created_at, updated_at";

impl<'a> Accounts<'a> {
    pub(crate) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn insert(&self, account: &ConnectedAccount) -> StoreResult<()> {
        sqlx::query(&format!(
            "INSERT INTO accounts ({COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(account.id.to_string())
        .bind(account.platform.as_str())
        .bind(&account.display_name)
        .bind(json(&account.capabilities))
        .bind(&account.credential_ref)
        .bind(ts(account.created_at))
        .bind(ts(account.updated_at))
        .execute(self.pool)
        .await?;
        Ok(())
    }

    pub async fn update(&self, account: &ConnectedAccount) -> StoreResult<()> {
        let result = sqlx::query(
            "UPDATE accounts SET display_name = ?, capabilities = ?, credential_ref = ?, \
             updated_at = ? WHERE id = ?",
        )
        .bind(&account.display_name)
        .bind(json(&account.capabilities))
        .bind(&account.credential_ref)
        .bind(ts(account.updated_at))
        .bind(account.id.to_string())
        .execute(self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(StoreError::not_found("account", account.id));
        }
        Ok(())
    }

    pub async fn get(&self, id: AccountId) -> StoreResult<Option<ConnectedAccount>> {
        sqlx::query(&format!("SELECT {COLUMNS} FROM accounts WHERE id = ?"))
            .bind(id.to_string())
            .fetch_optional(self.pool)
            .await?
            .as_ref()
            .map(decode)
            .transpose()
    }

    pub async fn require(&self, id: AccountId) -> StoreResult<ConnectedAccount> {
        self.get(id)
            .await?
            .ok_or_else(|| StoreError::not_found("account", id))
    }

    pub async fn list(&self) -> StoreResult<Vec<ConnectedAccount>> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM accounts ORDER BY platform, display_name"
        ))
        .fetch_all(self.pool)
        .await?
        .iter()
        .map(decode)
        .collect()
    }
}

fn decode(row: &SqliteRow) -> StoreResult<ConnectedAccount> {
    let platform = row.text("platform")?;
    Ok(ConnectedAccount {
        id: row.parsed("id")?,
        platform: Platform::parse(&platform)
            .ok_or_else(|| StoreError::Corrupt(format!("bad platform {platform:?}")))?,
        display_name: row.text("display_name")?,
        capabilities: row.json("capabilities")?,
        credential_ref: row.opt_text("credential_ref")?,
        created_at: row.time("created_at")?,
        updated_at: row.time("updated_at")?,
    })
}
