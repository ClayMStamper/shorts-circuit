//! Destination accounts and their envelope-encrypted credentials.

use chrono::{DateTime, Utc};
use shorts_cloud::{AccountPut, AccountStatus, CloudCredential, CredentialSource};
use shorts_domain::{AccountId, ConnectedAccount};
use sqlx::types::Json;
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use super::{DbError, DbResult, PgStore, to_i64, to_u64};
use crate::vault::Sealed;

/// A decrypted credential and its revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCredential {
    pub revision: u64,
    pub updated_by: CredentialSource,
    pub credential: CloudCredential,
}

fn context(account: AccountId) -> String {
    format!("credential:{account}")
}

fn source(raw: &str) -> DbResult<CredentialSource> {
    match raw {
        "desktop" => Ok(CredentialSource::Desktop),
        "runner" => Ok(CredentialSource::Runner),
        other => Err(DbError::Corrupt(format!("bad credential source {other:?}"))),
    }
}

fn source_str(s: CredentialSource) -> &'static str {
    match s {
        CredentialSource::Desktop => "desktop",
        CredentialSource::Runner => "runner",
    }
}

struct CredentialRow {
    revision: u64,
    updated_by: CredentialSource,
    updated_at: DateTime<Utc>,
    sealed: Sealed,
}

async fn load(
    conn: &mut PgConnection,
    account: AccountId,
    lock: bool,
) -> DbResult<Option<CredentialRow>> {
    let sql = format!(
        "SELECT revision, updated_by, key_id, wrapped_key, ciphertext, updated_at \
         FROM credentials WHERE account_id = $1{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let Some(row) = sqlx::query(&sql)
        .bind(account.as_uuid())
        .fetch_optional(conn)
        .await?
    else {
        return Ok(None);
    };
    Ok(Some(CredentialRow {
        revision: to_u64(row.try_get("revision")?, "credential revision")?,
        updated_by: source(&row.try_get::<String, _>("updated_by")?)?,
        updated_at: row.try_get("updated_at")?,
        sealed: Sealed {
            key_id: row.try_get("key_id")?,
            wrapped_key: row.try_get("wrapped_key")?,
            ciphertext: row.try_get("ciphertext")?,
        },
    }))
}

impl PgStore {
    fn open_credential(&self, account: AccountId, sealed: &Sealed) -> DbResult<CloudCredential> {
        let plain = self.vault.open(sealed, &context(account))?;
        serde_json::from_slice(&plain)
            .map_err(|_| DbError::Corrupt("a stored credential is not readable".into()))
    }

    async fn write_credential(
        &self,
        conn: &mut PgConnection,
        account: AccountId,
        credential: &CloudCredential,
        revision: u64,
        by: CredentialSource,
        now: DateTime<Utc>,
    ) -> DbResult<()> {
        let plain = serde_json::to_vec(credential).expect("credentials serialize");
        let sealed = self.vault.seal(&plain, &context(account));
        sqlx::query(
            "INSERT INTO credentials (account_id, revision, updated_by, key_id, wrapped_key, \
             ciphertext, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (account_id) DO UPDATE SET revision = EXCLUDED.revision, \
             updated_by = EXCLUDED.updated_by, key_id = EXCLUDED.key_id, \
             wrapped_key = EXCLUDED.wrapped_key, ciphertext = EXCLUDED.ciphertext, \
             updated_at = EXCLUDED.updated_at",
        )
        .bind(account.as_uuid())
        .bind(to_i64(revision))
        .bind(source_str(by))
        .bind(&sealed.key_id)
        .bind(&sealed.wrapped_key)
        .bind(&sealed.ciphertext)
        .bind(now)
        .execute(conn)
        .await?;
        Ok(())
    }

    /// Stores the account and the desktop's credential. Returns the credential status and
    /// whether the credential changed. Refused with [`DbError::CredentialConflict`] when the
    /// runner rotated the credential after `base_revision`.
    pub async fn put_account(
        &self,
        installation: Uuid,
        put: &AccountPut,
        now: DateTime<Utc>,
    ) -> DbResult<(AccountStatus, bool)> {
        let account = &put.account;
        if put.credential.platform() != account.platform {
            return Err(DbError::Refused(
                "the credential is for another platform".into(),
            ));
        }
        let mut tx = self.pool.begin().await?;
        let owner: Option<Uuid> =
            sqlx::query_scalar("SELECT installation_id FROM accounts WHERE id = $1 FOR UPDATE")
                .bind(account.id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?;
        match owner {
            Some(o) if o != installation => return Err(DbError::not_found("account", account.id)),
            Some(_) => {
                sqlx::query(
                    "UPDATE accounts SET platform = $2, record = $3, updated_at = $4 WHERE id = $1",
                )
                .bind(account.id.as_uuid())
                .bind(account.platform.as_str())
                .bind(Json(account))
                .bind(now)
                .execute(&mut *tx)
                .await?;
            }
            None => {
                sqlx::query(
                    "INSERT INTO accounts (id, installation_id, platform, record, updated_at) \
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(account.id.as_uuid())
                .bind(installation)
                .bind(account.platform.as_str())
                .bind(Json(account))
                .bind(now)
                .execute(&mut *tx)
                .await?;
            }
        }
        let current = load(&mut tx, account.id, true).await?;
        let (revision, updated_by, updated_at, changed) = match current {
            Some(row) => {
                let stored = self.open_credential(account.id, &row.sealed)?;
                if stored == put.credential {
                    (row.revision, row.updated_by, row.updated_at, false)
                } else if row.updated_by == CredentialSource::Runner
                    && put.base_revision.unwrap_or(0) < row.revision
                {
                    return Err(DbError::CredentialConflict {
                        revision: row.revision,
                    });
                } else {
                    let next = row.revision + 1;
                    self.write_credential(
                        &mut tx,
                        account.id,
                        &put.credential,
                        next,
                        CredentialSource::Desktop,
                        now,
                    )
                    .await?;
                    (next, CredentialSource::Desktop, now, true)
                }
            }
            None => {
                self.write_credential(
                    &mut tx,
                    account.id,
                    &put.credential,
                    1,
                    CredentialSource::Desktop,
                    now,
                )
                .await?;
                (1, CredentialSource::Desktop, now, true)
            }
        };
        tx.commit().await?;
        Ok((
            AccountStatus {
                account_id: account.id,
                credential_revision: revision,
                updated_by,
                updated_at,
            },
            changed,
        ))
    }

    pub async fn account(&self, id: AccountId) -> DbResult<Option<ConnectedAccount>> {
        let record: Option<Json<ConnectedAccount>> =
            sqlx::query_scalar("SELECT record FROM accounts WHERE id = $1")
                .bind(id.as_uuid())
                .fetch_optional(&self.pool)
                .await?;
        Ok(record.map(|r| r.0))
    }

    /// The installation's accounts and their credential revisions.
    pub async fn account_statuses(&self, installation: Uuid) -> DbResult<Vec<AccountStatus>> {
        let rows = sqlx::query(
            "SELECT a.id, c.revision, c.updated_by, c.updated_at FROM accounts a \
             JOIN credentials c ON c.account_id = a.id WHERE a.installation_id = $1 \
             ORDER BY a.id",
        )
        .bind(installation)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(AccountStatus {
                    account_id: AccountId::from_uuid(row.try_get("id")?),
                    credential_revision: to_u64(row.try_get("revision")?, "credential revision")?,
                    updated_by: source(&row.try_get::<String, _>("updated_by")?)?,
                    updated_at: row.try_get("updated_at")?,
                })
            })
            .collect()
    }

    /// Which installation an account belongs to.
    pub async fn account_installation(&self, id: AccountId) -> DbResult<Option<Uuid>> {
        Ok(
            sqlx::query_scalar("SELECT installation_id FROM accounts WHERE id = $1")
                .bind(id.as_uuid())
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn credential(&self, account: AccountId) -> DbResult<Option<StoredCredential>> {
        let mut conn = self.pool.acquire().await?;
        let Some(row) = load(&mut conn, account, false).await? else {
            return Ok(None);
        };
        Ok(Some(StoredCredential {
            revision: row.revision,
            updated_by: row.updated_by,
            credential: self.open_credential(account, &row.sealed)?,
        }))
    }

    /// Stores a credential the runner refreshed or renewed, unless the desktop replaced the
    /// credential since `read_revision` (its copy wins then). Returns the new revision.
    pub async fn save_rotated_credential(
        &self,
        account: AccountId,
        read_revision: u64,
        credential: &CloudCredential,
        now: DateTime<Utc>,
    ) -> DbResult<Option<u64>> {
        let mut tx = self.pool.begin().await?;
        let Some(row) = load(&mut tx, account, true).await? else {
            return Ok(None);
        };
        if row.revision != read_revision {
            return Ok(None);
        }
        let next = row.revision + 1;
        self.write_credential(
            &mut tx,
            account,
            credential,
            next,
            CredentialSource::Runner,
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(Some(next))
    }

    /// Re-wraps every credential's data key under the current master key. Returns how many
    /// were re-wrapped.
    pub async fn rewrap_credentials(&self) -> DbResult<usize> {
        let ids: Vec<Uuid> = sqlx::query_scalar("SELECT account_id FROM credentials")
            .fetch_all(&self.pool)
            .await?;
        let mut count = 0;
        for id in ids {
            let account = AccountId::from_uuid(id);
            let mut tx = self.pool.begin().await?;
            let Some(row) = load(&mut tx, account, true).await? else {
                continue;
            };
            if let Some(sealed) = self.vault.rewrap(&row.sealed, &context(account))? {
                sqlx::query(
                    "UPDATE credentials SET key_id = $2, wrapped_key = $3 WHERE account_id = $1",
                )
                .bind(id)
                .bind(&sealed.key_id)
                .bind(&sealed.wrapped_key)
                .execute(&mut *tx)
                .await?;
                count += 1;
            }
            tx.commit().await?;
        }
        Ok(count)
    }

    /// Master key ids still in use, for checking that a retired key can be dropped.
    pub async fn credential_key_ids(&self) -> DbResult<Vec<String>> {
        Ok(
            sqlx::query_scalar("SELECT DISTINCT key_id FROM credentials ORDER BY key_id")
                .fetch_all(&self.pool)
                .await?,
        )
    }
}
