use shorts_domain::{AssetId, Variant, VariantId};
use sqlx::sqlite::SqliteRow;
use sqlx::{SqliteConnection, SqlitePool};

use crate::codec::{RowExt, json, opt_json, ts};
use crate::{StoreError, StoreResult};

pub struct Variants<'a> {
    pool: &'a SqlitePool,
}

const COLUMNS: &str =
    "id, asset_id, media_file_id, name, recipe, recipe_revision, render, created_at, updated_at";

impl<'a> Variants<'a> {
    pub(crate) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn insert(&self, variant: &Variant) -> StoreResult<()> {
        let mut conn = self.pool.acquire().await?;
        insert_variant(&mut conn, variant).await
    }

    /// Writes `variant` if the stored row is still at `loaded_revision`, so a render of an old
    /// recipe can't overwrite a newer edit and two edits can't silently interleave.
    pub async fn update(&self, variant: &Variant, loaded_revision: u32) -> StoreResult<()> {
        let result = sqlx::query(
            "UPDATE variants SET name = ?, recipe = ?, recipe_revision = ?, render = ?, \
             updated_at = ? WHERE id = ? AND recipe_revision = ?",
        )
        .bind(&variant.name)
        .bind(json(variant.recipe()))
        .bind(i64::from(variant.revision()))
        .bind(opt_json(variant.render()))
        .bind(ts(variant.updated_at))
        .bind(variant.id.to_string())
        .bind(i64::from(loaded_revision))
        .execute(self.pool)
        .await?;
        if result.rows_affected() == 1 {
            return Ok(());
        }
        match self.get(variant.id).await? {
            Some(_) => Err(StoreError::conflict("variant", variant.id)),
            None => Err(StoreError::not_found("variant", variant.id)),
        }
    }

    pub async fn get(&self, id: VariantId) -> StoreResult<Option<Variant>> {
        sqlx::query(&format!("SELECT {COLUMNS} FROM variants WHERE id = ?"))
            .bind(id.to_string())
            .fetch_optional(self.pool)
            .await?
            .as_ref()
            .map(decode)
            .transpose()
    }

    pub async fn require(&self, id: VariantId) -> StoreResult<Variant> {
        self.get(id)
            .await?
            .ok_or_else(|| StoreError::not_found("variant", id))
    }

    pub async fn for_asset(&self, asset_id: AssetId) -> StoreResult<Vec<Variant>> {
        sqlx::query(&format!(
            "SELECT {COLUMNS} FROM variants WHERE asset_id = ? ORDER BY created_at"
        ))
        .bind(asset_id.to_string())
        .fetch_all(self.pool)
        .await?
        .iter()
        .map(decode)
        .collect()
    }
}

pub(crate) async fn insert_variant(
    conn: &mut SqliteConnection,
    variant: &Variant,
) -> StoreResult<()> {
    sqlx::query(&format!(
        "INSERT INTO variants ({COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
    ))
    .bind(variant.id.to_string())
    .bind(variant.asset_id.to_string())
    .bind(variant.media_file_id.to_string())
    .bind(&variant.name)
    .bind(json(variant.recipe()))
    .bind(i64::from(variant.revision()))
    .bind(opt_json(variant.render()))
    .bind(ts(variant.created_at))
    .bind(ts(variant.updated_at))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

fn decode(row: &SqliteRow) -> StoreResult<Variant> {
    Ok(Variant::restore(
        row.parsed("id")?,
        row.parsed("asset_id")?,
        row.parsed("media_file_id")?,
        row.text("name")?,
        row.json("recipe")?,
        row.u32("recipe_revision")?,
        row.opt_json("render")?,
        row.time("created_at")?,
        row.time("updated_at")?,
    ))
}
