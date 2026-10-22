use shorts_domain::{
    AccountId, CollectionId, CollectionRule, RuleId, UnattendedGrant, Workflow, WorkflowId,
};
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteRow;

use crate::codec::{RowExt, is_unique_violation, json, opt_ts, ts};
use crate::{StoreError, StoreResult};

const COLUMNS: &str =
    "id, rule_id, collection_id, account_id, settings, rights_confirmed_at, created_at, updated_at";

/// Workflows. Their rules and grants live in [`crate::Collections`]; creating a workflow writes
/// all three in one transaction.
pub struct Workflows<'a> {
    pool: &'a SqlitePool,
}

impl<'a> Workflows<'a> {
    pub(crate) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    /// Inserts the rule, the workflow and (for Auto Publish) the grant together. Fails with
    /// [`StoreError::DuplicateWorkflow`] when the collection already has a workflow to the
    /// account.
    pub async fn create(
        &self,
        rule: &CollectionRule,
        workflow: &Workflow,
        grant: Option<&UnattendedGrant>,
    ) -> StoreResult<()> {
        if rule.id != workflow.rule_id || rule.collection_id != workflow.collection_id {
            return Err(StoreError::Corrupt(
                "workflow does not match its rule".into(),
            ));
        }
        let mut tx = self.pool.begin().await?;
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
        .execute(&mut *tx)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO workflows ({COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(workflow.id.to_string())
        .bind(workflow.rule_id.to_string())
        .bind(workflow.collection_id.to_string())
        .bind(workflow.account_id.to_string())
        .bind(json(&workflow.settings))
        .bind(opt_ts(workflow.rights_confirmed_at))
        .bind(ts(workflow.created_at))
        .bind(ts(workflow.updated_at))
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                StoreError::DuplicateWorkflow {
                    collection: workflow.collection_id.to_string(),
                    account: workflow.account_id.to_string(),
                }
            } else {
                e.into()
            }
        })?;
        if let Some(g) = grant {
            sqlx::query(
                "INSERT INTO unattended_grants (collection_id, account_id, enabled_at, enabled_by) \
                 VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING",
            )
            .bind(g.collection_id.to_string())
            .bind(g.account_id.to_string())
            .bind(ts(g.enabled_at))
            .bind(&g.enabled_by)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn update(&self, w: &Workflow) -> StoreResult<()> {
        let result = sqlx::query(
            "UPDATE workflows SET settings = ?, rights_confirmed_at = ?, updated_at = ? \
             WHERE id = ?",
        )
        .bind(json(&w.settings))
        .bind(opt_ts(w.rights_confirmed_at))
        .bind(ts(w.updated_at))
        .bind(w.id.to_string())
        .execute(self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(StoreError::not_found("workflow", w.id));
        }
        Ok(())
    }

    /// Deletes the workflow, its rule and its grant. Publications and the firing log stay.
    pub async fn delete(&self, id: WorkflowId) -> StoreResult<()> {
        let w = self.require(id).await?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM unattended_grants WHERE collection_id = ? AND account_id = ?")
            .bind(w.collection_id.to_string())
            .bind(w.account_id.to_string())
            .execute(&mut *tx)
            .await?;
        // Cascades to the workflow row.
        sqlx::query("DELETE FROM collection_rules WHERE id = ?")
            .bind(w.rule_id.to_string())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM workflows WHERE id = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn get(&self, id: WorkflowId) -> StoreResult<Option<Workflow>> {
        sqlx::query(&format!("SELECT {COLUMNS} FROM workflows WHERE id = ?"))
            .bind(id.to_string())
            .fetch_optional(self.pool)
            .await?
            .as_ref()
            .map(decode)
            .transpose()
    }

    pub async fn require(&self, id: WorkflowId) -> StoreResult<Workflow> {
        self.get(id)
            .await?
            .ok_or_else(|| StoreError::not_found("workflow", id))
    }

    pub async fn for_rule(&self, rule: RuleId) -> StoreResult<Option<Workflow>> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM workflows WHERE rule_id = ?"
        ))
        .bind(rule.to_string())
        .fetch_optional(self.pool)
        .await?
        .as_ref()
        .map(decode)
        .transpose()
    }

    pub async fn find(
        &self,
        collection: CollectionId,
        account: AccountId,
    ) -> StoreResult<Option<Workflow>> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM workflows WHERE collection_id = ? AND account_id = ?"
        ))
        .bind(collection.to_string())
        .bind(account.to_string())
        .fetch_optional(self.pool)
        .await?
        .as_ref()
        .map(decode)
        .transpose()
    }

    /// Oldest first, which is the order the Workflows page lists them in.
    pub async fn list(&self) -> StoreResult<Vec<Workflow>> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM workflows ORDER BY created_at, id"
        ))
        .fetch_all(self.pool)
        .await?
        .iter()
        .map(decode)
        .collect()
    }
}

fn decode(row: &SqliteRow) -> StoreResult<Workflow> {
    Ok(Workflow {
        id: row.parsed("id")?,
        rule_id: row.parsed("rule_id")?,
        collection_id: row.parsed("collection_id")?,
        account_id: row.parsed("account_id")?,
        settings: row.json("settings")?,
        rights_confirmed_at: row.opt_time("rights_confirmed_at")?,
        created_at: row.time("created_at")?,
        updated_at: row.time("updated_at")?,
    })
}
