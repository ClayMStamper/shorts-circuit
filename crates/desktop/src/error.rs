use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use shorts_adapters::AdapterError;
use shorts_domain::TransitionError;
use shorts_domain::asset::AssetError;
use shorts_domain::collection::CollectionError;
use shorts_domain::job::JobError;
use shorts_domain::schedule::UnknownTimeZone;
use shorts_domain::variant::VariantError;
use shorts_media::MediaError;
use shorts_store::StoreError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Transition(#[from] TransitionError),
    #[error(transparent)]
    Variant(#[from] VariantError),
    #[error(transparent)]
    Asset(#[from] AssetError),
    #[error(transparent)]
    Collection(#[from] CollectionError),
    #[error(transparent)]
    Media(#[from] MediaError),
    #[error(transparent)]
    Adapter(#[from] AdapterError),
    #[error(transparent)]
    Job(#[from] JobError),
    #[error(transparent)]
    TimeZone(#[from] UnknownTimeZone),
    #[error("{0}")]
    Invalid(String),
    /// A source connector (TikTok, the OAuth broker) failed.
    #[error("{0}")]
    Intake(String),
    #[error("{0} not found")]
    NotFound(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The cloud runner couldn't be reached or refused.
    #[error(transparent)]
    Cloud(#[from] shorts_cloud::ClientError),
}

impl AppError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Store(StoreError::NotFound { .. }) | Self::NotFound(_) => "not_found",
            Self::Store(StoreError::Conflict { .. }) => "conflict",
            Self::Store(StoreError::DuplicateName { .. }) => "invalid",
            Self::Store(_) => "store",
            Self::Transition(_) => "transition",
            Self::Variant(_)
            | Self::Asset(_)
            | Self::Collection(_)
            | Self::TimeZone(_)
            | Self::Invalid(_) => "invalid",
            Self::Media(_) => "media",
            Self::Adapter(_) => "adapter",
            Self::Job(_) => "job",
            Self::Io(_) => "io",
            Self::Intake(_) => "intake",
            Self::Cloud(_) => "cloud",
        }
    }
}

/// Serialized for the UI as `{ "kind": ..., "message": ... }`.
impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("AppError", 2)?;
        s.serialize_field("kind", self.kind())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

pub type AppResult<T> = Result<T, AppError>;
