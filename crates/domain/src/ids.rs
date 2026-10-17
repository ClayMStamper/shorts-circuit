//! Typed identifiers. Each entity gets its own newtype so IDs can't be mixed up.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// A new time-ordered (UUIDv7) identifier.
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }
    };
}

id_type!(
    /// Identifies an [`Asset`](crate::Asset).
    AssetId
);
id_type!(
    /// Identifies a [`MediaFile`](crate::MediaFile).
    MediaFileId
);
id_type!(
    /// Identifies a [`Variant`](crate::Variant).
    VariantId
);
id_type!(
    /// Identifies a [`ConnectedAccount`](crate::ConnectedAccount).
    AccountId
);
id_type!(
    /// Identifies a [`Publication`](crate::Publication).
    PublicationId
);
id_type!(
    /// Identifies a [`Job`](crate::Job).
    JobId
);
id_type!(
    /// Identifies a [`PublishAttempt`](crate::PublishAttempt).
    AttemptId
);
id_type!(
    /// Identifies a [`Collection`](crate::Collection).
    CollectionId
);
id_type!(
    /// Identifies a [`CollectionRule`](crate::CollectionRule).
    RuleId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_strings() {
        let id = AssetId::new();
        let parsed: AssetId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn ids_are_time_ordered() {
        let a = JobId::new();
        let b = JobId::new();
        assert!(a < b);
    }

    #[test]
    fn ids_serialize_as_plain_strings() {
        let id = PublicationId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
    }
}
