use chrono::{DateTime, Utc};
use shorts_domain::{
    AccountId, AssetId, Collection, CollectionId, CollectionRule, PublicationId, RuleFiring,
    RuleId, UnattendedGrant,
};
use sqlx::sqlite::SqliteRow;
use sqlx::{SqliteConnection, SqlitePool};

use crate::codec::{RowExt, is_unique_violation, json, opt_ts, ts};
use crate::{StoreError, StoreResult};

/// Collections, their members and rules, unattended grants, and the rule firing log.
pub struct Collections<'a> {
    pool: &'a SqlitePool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionSummary {
    pub collection: Collection,
    pub member_count: u32,
}

impl<'a> Collections<'a> {
    pub(crate) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    /// Fails with [`StoreError::DuplicateName`] when another collection has the same name
    /// (ignoring case).
    pub async fn insert(&self, c: &Collection) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO collections (id, name, created_at, updated_at) VALUES (?, ?, ?, ?)",
        )
        .bind(c.id.to_string())
        .bind(&c.name)
        .bind(ts(c.created_at))
        .bind(ts(c.updated_at))
        .execute(self.pool)
        .await
        .map_err(|e| name_error(e, &c.name))?;
        Ok(())
    }

    pub async fn update(&self, c: &Collection) -> StoreResult<()> {
        let result = sqlx::query("UPDATE collections SET name = ?, updated_at = ? WHERE id = ?")
            .bind(&c.name)
            .bind(ts(c.updated_at))
            .bind(c.id.to_string())
            .execute(self.pool)
            .await
            .map_err(|e| name_error(e, &c.name))?;
        if result.rows_affected() == 0 {
            return Err(StoreError::not_found("collection", c.id));
        }
        Ok(())
    }

    /// Deletes the collection with its memberships, rules and grants. Assets, publications and
    /// the firing log are kept.
    pub async fn delete(&self, id: CollectionId) -> StoreResult<()> {
        let result = sqlx::query("DELETE FROM collections WHERE id = ?")
            .bind(id.to_string())
            .execute(self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(StoreError::not_found("collection", id));
        }
        Ok(())
    }

    pub async fn get(&self, id: CollectionId) -> StoreResult<Option<Collection>> {
        sqlx::query("SELECT id, name, created_at, updated_at FROM collections WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(self.pool)
            .await?
            .as_ref()
            .map(decode_collection)
            .transpose()
    }

    pub async fn require(&self, id: CollectionId) -> StoreResult<Collection> {
        self.get(id)
            .await?
            .ok_or_else(|| StoreError::not_found("collection", id))
    }

    /// By name.
    pub async fn list(&self) -> StoreResult<Vec<CollectionSummary>> {
        let rows = sqlx::query(
            "SELECT c.id, c.name, c.created_at, c.updated_at, \
             (SELECT COUNT(*) FROM collection_members m WHERE m.collection_id = c.id) AS members \
             FROM collections c ORDER BY c.name COLLATE NOCASE",
        )
        .fetch_all(self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(CollectionSummary {
                    collection: decode_collection(row)?,
                    member_count: row.u32("members")?,
                })
            })
            .collect()
    }

    /// Returns false when the asset was already a member.
    pub async fn add_member(
        &self,
        collection: CollectionId,
        asset: AssetId,
        at: DateTime<Utc>,
    ) -> StoreResult<bool> {
        let result = sqlx::query(
            "INSERT INTO collection_members (collection_id, asset_id, added_at) VALUES (?, ?, ?) \
             ON CONFLICT DO NOTHING",
        )
        .bind(collection.to_string())
        .bind(asset.to_string())
        .bind(ts(at))
        .execute(self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn remove_member(
        &self,
        collection: CollectionId,
        asset: AssetId,
    ) -> StoreResult<bool> {
        let result =
            sqlx::query("DELETE FROM collection_members WHERE collection_id = ? AND asset_id = ?")
                .bind(collection.to_string())
                .bind(asset.to_string())
                .execute(self.pool)
                .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Newest member first.
    pub async fn members(&self, collection: CollectionId) -> StoreResult<Vec<AssetId>> {
        let rows = sqlx::query(
            "SELECT asset_id FROM collection_members WHERE collection_id = ? \
             ORDER BY added_at DESC, asset_id",
        )
        .bind(collection.to_string())
        .fetch_all(self.pool)
        .await?;
        rows.iter().map(|r| r.parsed("asset_id")).collect()
    }

    /// When the asset entered the collection, if it is a member.
    pub async fn member_since(
        &self,
        collection: CollectionId,
        asset: AssetId,
    ) -> StoreResult<Option<DateTime<Utc>>> {
        let raw: Option<String> = sqlx::query_scalar(
            "SELECT added_at FROM collection_members WHERE collection_id = ? AND asset_id = ?",
        )
        .bind(collection.to_string())
        .bind(asset.to_string())
        .fetch_optional(self.pool)
        .await?;
        raw.as_deref().map(crate::codec::parse_ts).transpose()
    }

    /// The collections an asset belongs to, by name.
    pub async fn of_asset(&self, asset: AssetId) -> StoreResult<Vec<Collection>> {
        sqlx::query(
            "SELECT c.id, c.name, c.created_at, c.updated_at FROM collections c \
             JOIN collection_members m ON m.collection_id = c.id \
             WHERE m.asset_id = ? ORDER BY c.name COLLATE NOCASE",
        )
        .bind(asset.to_string())
        .fetch_all(self.pool)
        .await?
        .iter()
        .map(decode_collection)
        .collect()
    }

    // --- rules ------------------------------------------------------------------------------

    pub async fn insert_rule(&self, rule: &CollectionRule) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO collection_rules (id, collection_id, spec, effective_from, created_at, \
             updated_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(rule.id.to_string())
        .bind(rule.collection_id.to_string())
        .bind(json(&rule.spec))
        .bind(ts(rule.effective_from))
        .bind(ts(rule.created_at))
        .bind(ts(rule.updated_at))
        .execute(self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_rule(&self, rule: &CollectionRule) -> StoreResult<()> {
        let result = sqlx::query(
            "UPDATE collection_rules SET spec = ?, effective_from = ?, updated_at = ? WHERE id = ?",
        )
        .bind(json(&rule.spec))
        .bind(ts(rule.effective_from))
        .bind(ts(rule.updated_at))
        .bind(rule.id.to_string())
        .execute(self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(StoreError::not_found("rule", rule.id));
        }
        Ok(())
    }

    pub async fn delete_rule(&self, id: RuleId) -> StoreResult<()> {
        let result = sqlx::query("DELETE FROM collection_rules WHERE id = ?")
            .bind(id.to_string())
            .execute(self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(StoreError::not_found("rule", id));
        }
        Ok(())
    }

    pub async fn rule(&self, id: RuleId) -> StoreResult<Option<CollectionRule>> {
        sqlx::query(
            "SELECT id, collection_id, spec, effective_from, created_at, updated_at FROM collection_rules \
             WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(self.pool)
        .await?
        .as_ref()
        .map(decode_rule)
        .transpose()
    }

    pub async fn require_rule(&self, id: RuleId) -> StoreResult<CollectionRule> {
        self.rule(id)
            .await?
            .ok_or_else(|| StoreError::not_found("rule", id))
    }

    /// Oldest first, which is the order they fire in.
    pub async fn rules(&self, collection: CollectionId) -> StoreResult<Vec<CollectionRule>> {
        sqlx::query(
            "SELECT id, collection_id, spec, effective_from, created_at, updated_at FROM collection_rules \
             WHERE collection_id = ? ORDER BY created_at, id",
        )
        .bind(collection.to_string())
        .fetch_all(self.pool)
        .await?
        .iter()
        .map(decode_rule)
        .collect()
    }

    // --- unattended grants ------------------------------------------------------------------

    pub async fn set_grant(&self, grant: &UnattendedGrant) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO unattended_grants (collection_id, account_id, enabled_at, enabled_by) \
             VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(grant.collection_id.to_string())
        .bind(grant.account_id.to_string())
        .bind(ts(grant.enabled_at))
        .bind(&grant.enabled_by)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_grant(
        &self,
        collection: CollectionId,
        account: AccountId,
    ) -> StoreResult<()> {
        sqlx::query("DELETE FROM unattended_grants WHERE collection_id = ? AND account_id = ?")
            .bind(collection.to_string())
            .bind(account.to_string())
            .execute(self.pool)
            .await?;
        Ok(())
    }

    pub async fn grant(
        &self,
        collection: CollectionId,
        account: AccountId,
    ) -> StoreResult<Option<UnattendedGrant>> {
        sqlx::query(
            "SELECT collection_id, account_id, enabled_at, enabled_by FROM unattended_grants \
             WHERE collection_id = ? AND account_id = ?",
        )
        .bind(collection.to_string())
        .bind(account.to_string())
        .fetch_optional(self.pool)
        .await?
        .as_ref()
        .map(decode_grant)
        .transpose()
    }

    pub async fn grants(&self, collection: CollectionId) -> StoreResult<Vec<UnattendedGrant>> {
        sqlx::query(
            "SELECT collection_id, account_id, enabled_at, enabled_by FROM unattended_grants \
             WHERE collection_id = ? ORDER BY enabled_at",
        )
        .bind(collection.to_string())
        .fetch_all(self.pool)
        .await?
        .iter()
        .map(decode_grant)
        .collect()
    }

    // --- firings ----------------------------------------------------------------------------

    pub async fn firing_exists(
        &self,
        rule: RuleId,
        asset: AssetId,
        account: AccountId,
    ) -> StoreResult<bool> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM rule_firings WHERE rule_id = ? AND asset_id = ? AND account_id = ?",
        )
        .bind(rule.to_string())
        .bind(asset.to_string())
        .bind(account.to_string())
        .fetch_one(self.pool)
        .await?;
        Ok(n > 0)
    }

    pub async fn firings_for_asset(&self, asset: AssetId) -> StoreResult<Vec<RuleFiring>> {
        sqlx::query(&format!(
            "SELECT {FIRING_COLUMNS} FROM rule_firings WHERE asset_id = ? ORDER BY fired_at"
        ))
        .bind(asset.to_string())
        .fetch_all(self.pool)
        .await?
        .iter()
        .map(decode_firing)
        .collect()
    }

    /// Everything a rule created, newest first.
    pub async fn firings_for_rule(&self, rule: RuleId) -> StoreResult<Vec<RuleFiring>> {
        sqlx::query(&format!(
            "SELECT {FIRING_COLUMNS} FROM rule_firings WHERE rule_id = ? ORDER BY fired_at DESC"
        ))
        .bind(rule.to_string())
        .fetch_all(self.pool)
        .await?
        .iter()
        .map(decode_firing)
        .collect()
    }

    pub async fn firing_for_publication(
        &self,
        id: PublicationId,
    ) -> StoreResult<Option<RuleFiring>> {
        sqlx::query(&format!(
            "SELECT {FIRING_COLUMNS} FROM rule_firings WHERE publication_id = ?"
        ))
        .bind(id.to_string())
        .fetch_optional(self.pool)
        .await?
        .as_ref()
        .map(decode_firing)
        .transpose()
    }

    pub async fn fired_count(&self, rule: RuleId) -> StoreResult<u32> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rule_firings WHERE rule_id = ?")
            .bind(rule.to_string())
            .fetch_one(self.pool)
            .await?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// Marks an unattended firing as approved by its rule. Returns false when it already was,
    /// so concurrent callers approve at most once.
    pub async fn mark_auto_approved(
        &self,
        id: PublicationId,
        at: DateTime<Utc>,
    ) -> StoreResult<bool> {
        let result = sqlx::query(
            "UPDATE rule_firings SET auto_approved_at = ? \
             WHERE publication_id = ? AND unattended = 1 AND auto_approved_at IS NULL",
        )
        .bind(opt_ts(Some(at)))
        .bind(id.to_string())
        .execute(self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }
}

const FIRING_COLUMNS: &str =
    "rule_id, asset_id, account_id, publication_id, unattended, auto_approved_at, fired_at";

/// Inserts a firing inside a caller's transaction. A duplicate `(rule, asset, account)` is
/// reported as `Ok(false)`.
pub(crate) async fn insert_firing(
    conn: &mut SqliteConnection,
    firing: &RuleFiring,
) -> StoreResult<bool> {
    let result = sqlx::query(&format!(
        "INSERT INTO rule_firings ({FIRING_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?)"
    ))
    .bind(firing.rule_id.to_string())
    .bind(firing.asset_id.to_string())
    .bind(firing.account_id.to_string())
    .bind(firing.publication_id.to_string())
    .bind(i64::from(firing.unattended))
    .bind(opt_ts(firing.auto_approved_at))
    .bind(ts(firing.fired_at))
    .execute(&mut *conn)
    .await;
    match result {
        Ok(_) => Ok(true),
        Err(e) if is_unique_violation(&e) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn name_error(e: sqlx::Error, name: &str) -> StoreError {
    if is_unique_violation(&e) {
        StoreError::DuplicateName {
            name: name.to_owned(),
        }
    } else {
        e.into()
    }
}

fn decode_collection(row: &SqliteRow) -> StoreResult<Collection> {
    Ok(Collection {
        id: row.parsed("id")?,
        name: row.text("name")?,
        created_at: row.time("created_at")?,
        updated_at: row.time("updated_at")?,
    })
}

fn decode_rule(row: &SqliteRow) -> StoreResult<CollectionRule> {
    Ok(CollectionRule {
        id: row.parsed("id")?,
        collection_id: row.parsed("collection_id")?,
        spec: row.json("spec")?,
        effective_from: row.time("effective_from")?,
        created_at: row.time("created_at")?,
        updated_at: row.time("updated_at")?,
    })
}

fn decode_grant(row: &SqliteRow) -> StoreResult<UnattendedGrant> {
    Ok(UnattendedGrant {
        collection_id: row.parsed("collection_id")?,
        account_id: row.parsed("account_id")?,
        enabled_at: row.time("enabled_at")?,
        enabled_by: row.text("enabled_by")?,
    })
}

fn decode_firing(row: &SqliteRow) -> StoreResult<RuleFiring> {
    Ok(RuleFiring {
        rule_id: row.parsed("rule_id")?,
        asset_id: row.parsed("asset_id")?,
        account_id: row.parsed("account_id")?,
        publication_id: row.parsed("publication_id")?,
        unattended: row.int("unattended")? != 0,
        auto_approved_at: row.opt_time("auto_approved_at")?,
        fired_at: row.time("fired_at")?,
    })
}
