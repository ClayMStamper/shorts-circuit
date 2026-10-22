-- TikTok posts noticed through the Display API (metadata only; originals come from the watch
-- folder), and match suggestions the user turned down.

CREATE TABLE tiktok_posts (
    video_id      TEXT PRIMARY KEY NOT NULL,
    -- The awaiting-media item, or after a confirmed match the asset with the original.
    asset_id      TEXT NOT NULL REFERENCES assets (id),
    posted_at     TEXT NOT NULL,
    duration_ms   INTEGER,
    title         TEXT NOT NULL,
    caption       TEXT NOT NULL,
    share_url     TEXT,
    cover_url     TEXT,
    cover_path    TEXT,
    discovered_at TEXT NOT NULL,
    matched_at    TEXT
);
CREATE INDEX tiktok_posts_by_asset ON tiktok_posts (asset_id);
CREATE INDEX tiktok_posts_by_time ON tiktok_posts (posted_at);

CREATE TABLE tiktok_match_dismissals (
    video_id     TEXT NOT NULL REFERENCES tiktok_posts (video_id),
    asset_id     TEXT NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    dismissed_at TEXT NOT NULL,
    PRIMARY KEY (video_id, asset_id)
);
