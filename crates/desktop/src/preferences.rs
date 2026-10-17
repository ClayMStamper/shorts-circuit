use serde::{Deserialize, Serialize};
use shorts_domain::{CollectionId, OverduePolicy, TimeZoneName};
use shorts_scheduler::{PostingPlan, PostingSlot};

use crate::app::App;
use crate::error::{AppError, AppResult};

const TIMEZONE: &str = "timezone";
const OVERDUE: &str = "overdue_policy";
const WATCH_FOLDER: &str = "watch_folder";
const EXPORT_DIR: &str = "export_dir";
const POSTING_SLOTS: &str = "posting_slots";
const WATCH_COLLECTION: &str = "watch_collection";

/// User preferences. The posting slots are interpreted in `timezone`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    pub timezone: TimeZoneName,
    pub overdue_policy: OverduePolicy,
    pub watch_folder: Option<String>,
    pub export_dir: Option<String>,
    pub posting_slots: Vec<PostingSlot>,
    /// Watch-folder imports are added to this collection (and its rules run).
    pub watch_collection: Option<CollectionId>,
}

impl Preferences {
    pub fn posting_plan(&self) -> PostingPlan {
        PostingPlan {
            timezone: self.timezone,
            slots: self.posting_slots.clone(),
        }
    }
}

/// Fields left as `None` are unchanged. `watch_folder` / `export_dir` use `Some(None)` to clear.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreferencesUpdate {
    pub timezone: Option<String>,
    pub overdue_policy: Option<OverduePolicy>,
    #[serde(default, with = "double_option")]
    pub watch_folder: Option<Option<String>>,
    #[serde(default, with = "double_option")]
    pub export_dir: Option<Option<String>>,
    pub posting_slots: Option<Vec<PostingSlot>>,
    #[serde(default, with = "double_option")]
    pub watch_collection: Option<Option<CollectionId>>,
}

mod double_option {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Ok(Some(Option::deserialize(d)?))
    }
}

impl App {
    pub async fn preferences(&self) -> AppResult<Preferences> {
        let settings = self.store.settings();
        Ok(Preferences {
            timezone: settings
                .get(TIMEZONE)
                .await?
                .unwrap_or_else(TimeZoneName::utc),
            overdue_policy: settings.get(OVERDUE).await?.unwrap_or_default(),
            watch_folder: settings.get(WATCH_FOLDER).await?.flatten(),
            export_dir: settings.get(EXPORT_DIR).await?.flatten(),
            posting_slots: settings.get(POSTING_SLOTS).await?.unwrap_or_default(),
            watch_collection: settings.get(WATCH_COLLECTION).await?.flatten(),
        })
    }

    pub(crate) async fn clear_watch_collection(&self) -> AppResult<()> {
        self.store
            .settings()
            .set(WATCH_COLLECTION, &None::<CollectionId>)
            .await?;
        Ok(())
    }

    /// Records the OS timezone on first launch; an explicit user choice is never overwritten.
    pub async fn ensure_timezone(&self, detected: &str) -> AppResult<()> {
        let settings = self.store.settings();
        if settings.get::<TimeZoneName>(TIMEZONE).await?.is_none()
            && let Ok(tz) = TimeZoneName::parse(detected)
        {
            settings.set(TIMEZONE, &tz).await?;
        }
        Ok(())
    }

    pub async fn update_preferences(&self, update: PreferencesUpdate) -> AppResult<Preferences> {
        let settings = self.store.settings();
        if let Some(tz) = update.timezone {
            settings.set(TIMEZONE, &TimeZoneName::parse(&tz)?).await?;
        }
        if let Some(policy) = update.overdue_policy {
            settings.set(OVERDUE, &policy).await?;
            if let Some(worker) = self.running_worker() {
                worker.set_overdue_policy(policy);
            }
        }
        if let Some(folder) = update.watch_folder {
            if let Some(f) = &folder
                && !std::path::Path::new(f).is_dir()
            {
                return Err(AppError::Invalid(format!("{f} is not a folder")));
            }
            settings.set(WATCH_FOLDER, &folder).await?;
        }
        if let Some(dir) = update.export_dir {
            settings.set(EXPORT_DIR, &dir).await?;
        }
        if let Some(slots) = update.posting_slots {
            settings.set(POSTING_SLOTS, &slots).await?;
        }
        if let Some(collection) = update.watch_collection {
            if let Some(id) = collection {
                self.store.collections().require(id).await?;
            }
            settings.set(WATCH_COLLECTION, &collection).await?;
            self.emit(crate::events::AppEvent::CollectionsChanged);
        }
        self.preferences().await
    }
}
