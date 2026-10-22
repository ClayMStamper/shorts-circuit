//! Scheduled instants. Instants are always UTC; the user's IANA timezone is kept alongside so
//! the time can be shown and recurring slots computed in local time.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("unknown IANA timezone {0:?}")]
pub struct UnknownTimeZone(pub String);

/// A validated IANA timezone name such as `America/New_York`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TimeZoneName(Tz);

impl TimeZoneName {
    pub fn parse(name: &str) -> Result<Self, UnknownTimeZone> {
        Tz::from_str(name.trim())
            .map(Self)
            .map_err(|_| UnknownTimeZone(name.to_owned()))
    }

    pub fn utc() -> Self {
        Self(Tz::UTC)
    }

    pub fn tz(&self) -> Tz {
        self.0
    }

    pub fn name(&self) -> &'static str {
        self.0.name()
    }
}

impl TryFrom<String> for TimeZoneName {
    type Error = UnknownTimeZone;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<TimeZoneName> for String {
    fn from(tz: TimeZoneName) -> Self {
        tz.name().to_owned()
    }
}

impl fmt::Display for TimeZoneName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// When a publication should go out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// The execution instant, UTC.
    pub at: DateTime<Utc>,
    /// The timezone the user picked the time in.
    pub timezone: TimeZoneName,
}

impl Schedule {
    pub fn new(at: DateTime<Utc>, timezone: TimeZoneName) -> Self {
        Self { at, timezone }
    }

    /// The instant in the user's timezone.
    pub fn local(&self) -> DateTime<Tz> {
        self.at.with_timezone(&self.timezone.tz())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timezone_names_are_validated() {
        assert_eq!(
            TimeZoneName::parse("Europe/Berlin").unwrap().name(),
            "Europe/Berlin"
        );
        assert!(TimeZoneName::parse("Mars/Olympus").is_err());
        assert!(serde_json::from_str::<TimeZoneName>("\"Nope\"").is_err());
    }

    #[test]
    fn schedule_renders_in_local_time() {
        let at = DateTime::parse_from_rfc3339("2026-07-01T16:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let schedule = Schedule::new(at, TimeZoneName::parse("America/Chicago").unwrap());
        assert_eq!(schedule.local().to_rfc3339(), "2026-07-01T11:00:00-05:00");
    }
}
