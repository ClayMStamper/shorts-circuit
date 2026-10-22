//! Desktop installations and their bearer tokens. Only a token's SHA-256 is stored.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use super::{DbError, DbResult, PgStore};

const TOKEN_PREFIX: &str = "sc_";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.trim().as_bytes()))
}

fn decode(row: &sqlx::postgres::PgRow) -> DbResult<Installation> {
    Ok(Installation {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        created_at: row.try_get("created_at")?,
        last_seen_at: row.try_get("last_seen_at")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}

impl PgStore {
    /// Registers an installation and returns it with its token, which is shown once and
    /// never stored.
    pub async fn create_installation(
        &self,
        name: &str,
        now: DateTime<Utc>,
    ) -> DbResult<(Installation, String)> {
        let name = name.trim();
        if name.is_empty() {
            return Err(DbError::Refused("give the installation a name".into()));
        }
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let token = format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));
        let installation = Installation {
            id: Uuid::now_v7(),
            name: name.to_owned(),
            created_at: now,
            last_seen_at: None,
            revoked_at: None,
        };
        sqlx::query(
            "INSERT INTO installations (id, name, token_sha256, created_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(installation.id)
        .bind(&installation.name)
        .bind(token_hash(&token))
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok((installation, token))
    }

    /// The installation a bearer token belongs to, unless it was revoked.
    pub async fn authenticate(
        &self,
        token: &str,
        now: DateTime<Utc>,
    ) -> DbResult<Option<Installation>> {
        if !token.starts_with(TOKEN_PREFIX) {
            return Ok(None);
        }
        let row = sqlx::query(
            "UPDATE installations SET last_seen_at = $2 \
             WHERE token_sha256 = $1 AND revoked_at IS NULL \
             RETURNING id, name, created_at, last_seen_at, revoked_at",
        )
        .bind(token_hash(token))
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(decode).transpose()
    }

    pub async fn installations(&self) -> DbResult<Vec<Installation>> {
        sqlx::query(
            "SELECT id, name, created_at, last_seen_at, revoked_at FROM installations \
             ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(decode)
        .collect()
    }

    /// Revokes an installation's token. Its publications stay as they are.
    pub async fn revoke_installation(&self, id: Uuid, now: DateTime<Utc>) -> DbResult<()> {
        let done = sqlx::query(
            "UPDATE installations SET revoked_at = $2 WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(id)
        .bind(now)
        .execute(&self.pool)
        .await?;
        if done.rows_affected() == 0 {
            return Err(DbError::not_found("installation", id));
        }
        Ok(())
    }
}
