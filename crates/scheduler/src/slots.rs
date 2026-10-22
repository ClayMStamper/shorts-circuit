//! Recurring posting slots in the user's timezone, resolved to UTC instants.
//!
//! Wall-clock times that don't exist (spring-forward gap) are moved forward by the length of
//! the gap, e.g. 02:30 on the US spring-forward day becomes 03:30 local. Times that occur twice
//! (fall-back overlap) resolve to the first occurrence.

use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, Offset,
    TimeZone, Utc, Weekday,
};
use serde::{Deserialize, Serialize};
use shorts_domain::TimeZoneName;

/// Converts a local wall-clock time in `tz` to UTC using the gap/overlap rules above.
pub fn resolve_local(tz: TimeZoneName, local: NaiveDateTime) -> DateTime<Utc> {
    let zone = tz.tz();
    match zone.from_local_datetime(&local) {
        LocalResult::Single(t) => t.with_timezone(&Utc),
        LocalResult::Ambiguous(first, _) => first.with_timezone(&Utc),
        LocalResult::None => {
            // In a gap: interpret the wall time with the offset in force just before it, which
            // lands the same distance past the end of the gap.
            let before = (local - Duration::hours(12)).and_utc();
            let offset = zone.offset_from_utc_datetime(&before.naive_utc()).fix();
            (local - Duration::seconds(i64::from(offset.local_minus_utc()))).and_utc()
        }
    }
}

/// A weekly recurring time, e.g. Mon/Wed/Fri at 18:00.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostingSlot {
    /// Empty means every day.
    pub weekdays: Vec<Weekday>,
    pub time: NaiveTime,
}

impl PostingSlot {
    fn runs_on(&self, day: Weekday) -> bool {
        self.weekdays.is_empty() || self.weekdays.contains(&day)
    }
}

/// The user's posting slots and the timezone they are expressed in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostingPlan {
    pub timezone: TimeZoneName,
    pub slots: Vec<PostingSlot>,
}

impl PostingPlan {
    /// Slot occurrences strictly after `after`, in order, looking at most `days` ahead.
    pub fn occurrences_after(&self, after: DateTime<Utc>, days: u32) -> Vec<DateTime<Utc>> {
        let start: NaiveDate = after.with_timezone(&self.timezone.tz()).date_naive();
        let mut out = Vec::new();
        for offset in 0..=days {
            let Some(date) = start.checked_add_days(chrono::Days::new(u64::from(offset))) else {
                break;
            };
            for slot in self.slots.iter().filter(|s| s.runs_on(date.weekday())) {
                let at = resolve_local(self.timezone, date.and_time(slot.time));
                if at > after {
                    out.push(at);
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }

    /// The first slot after `after`.
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.occurrences_after(after, 8).into_iter().next()
    }

    /// The first slot after `after` that is at least `min_gap` away from every `taken` instant
    /// (e.g. other publications to the same account). Looks up to `horizon_days` ahead.
    pub fn next_free(
        &self,
        after: DateTime<Utc>,
        taken: &[DateTime<Utc>],
        min_gap: Duration,
        horizon_days: u32,
    ) -> Option<DateTime<Utc>> {
        self.occurrences_after(after, horizon_days)
            .into_iter()
            .find(|slot| taken.iter().all(|t| (*t - *slot).abs() >= min_gap))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn local(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn new_york() -> TimeZoneName {
        TimeZoneName::parse("America/New_York").unwrap()
    }

    fn daily(time: &str) -> PostingPlan {
        PostingPlan {
            timezone: new_york(),
            slots: vec![PostingSlot {
                weekdays: vec![],
                time: NaiveTime::parse_from_str(time, "%H:%M").unwrap(),
            }],
        }
    }

    #[test]
    fn spring_forward_gap_moves_later_by_the_gap() {
        // 2026-03-08 02:00 EST -> 03:00 EDT; 02:30 doesn't exist.
        let at = resolve_local(new_york(), local("2026-03-08 02:30"));
        assert_eq!(at, utc("2026-03-08T07:30:00Z"));
        assert_eq!(
            at.with_timezone(&new_york().tz()).to_rfc3339(),
            "2026-03-08T03:30:00-04:00"
        );
    }

    #[test]
    fn fall_back_overlap_uses_the_first_occurrence() {
        // 2026-11-01 02:00 EDT -> 01:00 EST; 01:30 happens twice.
        let at = resolve_local(new_york(), local("2026-11-01 01:30"));
        assert_eq!(at, utc("2026-11-01T05:30:00Z"));
    }

    #[test]
    fn daily_slot_keeps_local_time_across_fall_back() {
        let plan = daily("09:00");
        let slots = plan.occurrences_after(utc("2026-10-30T00:00:00Z"), 4);
        assert_eq!(
            slots[..4],
            [
                utc("2026-10-30T13:00:00Z"),
                utc("2026-10-31T13:00:00Z"),
                // EST from here on: same 09:00 local, one hour later in UTC.
                utc("2026-11-01T14:00:00Z"),
                utc("2026-11-02T14:00:00Z"),
            ]
        );
    }

    #[test]
    fn daily_slot_keeps_local_time_across_spring_forward() {
        let plan = daily("09:00");
        let slots = plan.occurrences_after(utc("2026-03-07T00:00:00Z"), 2);
        assert_eq!(
            slots,
            vec![utc("2026-03-07T14:00:00Z"), utc("2026-03-08T13:00:00Z")]
        );
    }

    #[test]
    fn slot_inside_the_gap_still_fires_once() {
        let plan = daily("02:30");
        let slots = plan.occurrences_after(utc("2026-03-07T12:00:00Z"), 3);
        assert_eq!(
            slots,
            vec![
                utc("2026-03-08T07:30:00Z"),
                utc("2026-03-09T06:30:00Z"),
                utc("2026-03-10T06:30:00Z"),
            ]
        );
    }

    #[test]
    fn slot_inside_the_overlap_fires_once() {
        let plan = daily("01:30");
        let slots = plan.occurrences_after(utc("2026-10-31T12:00:00Z"), 2);
        assert_eq!(
            slots,
            vec![utc("2026-11-01T05:30:00Z"), utc("2026-11-02T06:30:00Z")]
        );
    }

    #[test]
    fn southern_hemisphere_dst() {
        // Australia/Sydney springs forward on 2026-10-04 at 02:00 -> 03:00.
        let tz = TimeZoneName::parse("Australia/Sydney").unwrap();
        assert_eq!(
            resolve_local(tz, local("2026-10-04 02:15")),
            utc("2026-10-03T16:15:00Z")
        );
    }

    #[test]
    fn weekday_filters_and_free_slots() {
        let plan = PostingPlan {
            timezone: TimeZoneName::parse("Europe/London").unwrap(),
            slots: vec![
                PostingSlot {
                    weekdays: vec![Weekday::Mon, Weekday::Wed],
                    time: NaiveTime::from_hms_opt(18, 0, 0).unwrap(),
                },
                PostingSlot {
                    weekdays: vec![Weekday::Mon],
                    time: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
                },
            ],
        };
        // Saturday 2026-10-17.
        let after = utc("2026-10-17T12:00:00Z");
        assert_eq!(plan.next_after(after), Some(utc("2026-10-19T07:00:00Z")));
        let taken = [utc("2026-10-19T07:00:00Z"), utc("2026-10-19T17:00:00Z")];
        assert_eq!(
            plan.next_free(after, &taken, Duration::hours(1), 14),
            Some(utc("2026-10-21T17:00:00Z"))
        );
        // London leaves BST on 2026-10-25: Monday 26th 18:00 is 18:00Z.
        let later = plan.occurrences_after(utc("2026-10-25T00:00:00Z"), 1);
        assert_eq!(
            later,
            vec![utc("2026-10-26T08:00:00Z"), utc("2026-10-26T18:00:00Z")]
        );
    }

    #[test]
    fn empty_plan_has_no_slots() {
        let plan = PostingPlan {
            timezone: TimeZoneName::utc(),
            slots: vec![],
        };
        assert_eq!(plan.next_after(utc("2026-01-01T00:00:00Z")), None);
    }
}
