-- Provider API quota spent per day, so the app can stop before a daily quota runs out and warn
-- in the UI. `day` is the provider's quota day (YouTube: the date in Pacific time).

CREATE TABLE quota_usage (
    bucket     TEXT NOT NULL,
    day        TEXT NOT NULL,
    units      INTEGER NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (bucket, day)
);
