//! SQLite persistence: migrations, repositories, WAL configuration.
//!
//! The store owns all SQL. It persists domain values as they are; it never decides a state
//! transition itself. Mutable rows that workers and the UI can both touch (publications, jobs)
//! are written with optimistic concurrency on a version column.

mod accounts;
mod assets;
mod attempts;
mod codec;
mod collections;
mod jobs;
mod media_files;
mod publications;
mod quota;
mod settings;
mod tiktok;
mod variants;

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use thiserror::Error;

pub use accounts::Accounts;
pub use assets::{AssetFilter, Assets};
pub use attempts::Attempts;
pub use collections::{CollectionSummary, Collections};
pub use jobs::{JobFilter, Jobs};
pub use media_files::MediaFiles;
pub use publications::{PublicationFilter, PublicationLogEntry, Publications};
pub use quota::Quota;
pub use settings::Settings;
pub use tiktok::TikTokPosts;
pub use variants::Variants;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("{entity} {id} not found")]
    NotFound { entity: &'static str, id: String },
    /// Someone else changed the row since it was read.
    #[error("{entity} {id} was modified concurrently")]
    Conflict { entity: &'static str, id: String },
    #[error("a file with checksum {checksum} is already in the library")]
    DuplicateMedia { checksum: String },
    #[error("the name {name:?} is already taken")]
    DuplicateName { name: String },
    #[error("an open {kind} job already exists for {subject}")]
    DuplicateOpenJob { kind: String, subject: String },
    /// A job transition was refused, e.g. the caller no longer holds the lease.
    #[error(transparent)]
    Job(#[from] shorts_domain::job::JobError),
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
}

impl StoreError {
    pub(crate) fn not_found(entity: &'static str, id: impl ToString) -> Self {
        Self::NotFound {
            entity,
            id: id.to_string(),
        }
    }

    pub(crate) fn conflict(entity: &'static str, id: impl ToString) -> Self {
        Self::Conflict {
            entity,
            id: id.to_string(),
        }
    }
}

impl StoreError {
    /// The caller's lease is gone: the job was reclaimed, cancelled or finished elsewhere.
    pub fn is_lease_lost(&self) -> bool {
        use shorts_domain::job::JobError;
        matches!(
            self,
            Self::Job(JobError::LeaseNotHeld { .. } | JobError::NotAllowed { .. })
        )
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

/// Handle to the application database. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Opens (creating if needed) the database at `path` in WAL mode and runs migrations.
    pub async fn open(path: &Path) -> StoreResult<Self> {
        let options = SqliteConnectOptions::from_str("sqlite:")?
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(10));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;
        MIGRATOR.run(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// The active journal mode, e.g. `"wal"`.
    pub async fn journal_mode(&self) -> StoreResult<String> {
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&self.pool)
            .await?;
        Ok(mode)
    }

    pub fn assets(&self) -> Assets<'_> {
        Assets::new(&self.pool)
    }

    pub fn media_files(&self) -> MediaFiles<'_> {
        MediaFiles::new(&self.pool)
    }

    pub fn variants(&self) -> Variants<'_> {
        Variants::new(&self.pool)
    }

    pub fn accounts(&self) -> Accounts<'_> {
        Accounts::new(&self.pool)
    }

    pub fn publications(&self) -> Publications<'_> {
        Publications::new(&self.pool)
    }

    pub fn jobs(&self) -> Jobs<'_> {
        Jobs::new(&self.pool)
    }

    pub fn attempts(&self) -> Attempts<'_> {
        Attempts::new(&self.pool)
    }

    pub fn collections(&self) -> Collections<'_> {
        Collections::new(&self.pool)
    }

    pub fn tiktok_posts(&self) -> TikTokPosts<'_> {
        TikTokPosts::new(&self.pool)
    }

    pub fn quota(&self) -> Quota<'_> {
        Quota::new(&self.pool)
    }

    pub fn settings(&self) -> Settings<'_> {
        Settings::new(&self.pool)
    }

    /// Records an import in one transaction: the asset (inserted when `new_asset`, otherwise
    /// updated, e.g. a saved link that now has media), its original and a first variant.
    pub async fn save_import(
        &self,
        asset: &shorts_domain::Asset,
        new_asset: bool,
        media: &shorts_domain::MediaFile,
        variant: &shorts_domain::Variant,
    ) -> StoreResult<()> {
        let mut tx = self.pool.begin().await?;
        if new_asset {
            assets::insert_asset(&mut tx, asset).await?;
        } else {
            assets::update_asset(&mut tx, asset).await?;
        }
        media_files::insert_media_file(&mut tx, media).await?;
        variants::insert_variant(&mut tx, variant).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Records a newly noticed TikTok post with its awaiting-media item. Returns false, writing
    /// nothing, when the post is already known.
    pub async fn record_tiktok_post(
        &self,
        asset: &shorts_domain::Asset,
        post: &shorts_domain::TikTokPost,
    ) -> StoreResult<bool> {
        if post.asset_id != asset.id {
            return Err(StoreError::Corrupt(
                "TikTok post does not match its item".into(),
            ));
        }
        let mut tx = self.pool.begin().await?;
        assets::insert_asset(&mut tx, asset).await?;
        if !tiktok::insert_post(&mut tx, post).await? {
            tx.rollback().await?;
            return Ok(false);
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Confirms that `target` holds the original of TikTok post `video_id`: `target` (already
    /// carrying the post's details) takes over the post and the awaiting item's collections,
    /// and the awaiting item is removed. All or nothing.
    pub async fn merge_tiktok_match(
        &self,
        video_id: &str,
        awaiting: shorts_domain::AssetId,
        target: &shorts_domain::Asset,
        at: chrono::DateTime<chrono::Utc>,
    ) -> StoreResult<()> {
        let mut tx = self.pool.begin().await?;
        tiktok::merge(&mut tx, video_id, awaiting, target, at).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Records that a rule created `publication`, inserting both in one transaction. Returns
    /// false, writing nothing, when the rule already fired for this asset and account.
    pub async fn record_rule_firing(
        &self,
        firing: &shorts_domain::RuleFiring,
        publication: &shorts_domain::Publication,
    ) -> StoreResult<bool> {
        if firing.publication_id != publication.id() || firing.asset_id != publication.asset_id() {
            return Err(StoreError::Corrupt(
                "rule firing does not match its publication".into(),
            ));
        }
        let mut tx = self.pool.begin().await?;
        publications::insert_publication(&mut tx, publication).await?;
        if !collections::insert_firing(&mut tx, firing).await? {
            tx.rollback().await?;
            return Ok(false);
        }
        tx.commit().await?;
        Ok(true)
    }
}
