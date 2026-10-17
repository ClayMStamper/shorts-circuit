-- Shorts Circuit schema, version 1.
-- Instants are UTC text in a fixed-width format (YYYY-MM-DDTHH:MM:SS.fffffffffZ) so they compare
-- correctly as strings. JSON columns hold serde-serialized domain values.

CREATE TABLE assets (
    id               TEXT PRIMARY KEY NOT NULL,
    title            TEXT NOT NULL,
    creator          TEXT,
    source_url       TEXT,
    notes            TEXT NOT NULL DEFAULT '',
    rating           INTEGER CHECK (rating BETWEEN 1 AND 5),
    video_permission TEXT NOT NULL,
    audio_permission TEXT NOT NULL,
    status           TEXT NOT NULL CHECK (status IN ('awaiting_media', 'ready')),
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);

CREATE TABLE asset_tags (
    asset_id TEXT NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    tag      TEXT NOT NULL,
    PRIMARY KEY (asset_id, tag)
);
CREATE INDEX asset_tags_by_tag ON asset_tags (tag);

CREATE TABLE media_files (
    id            TEXT PRIMARY KEY NOT NULL,
    asset_id      TEXT NOT NULL REFERENCES assets (id),
    path          TEXT NOT NULL,
    original_name TEXT NOT NULL,
    checksum      TEXT NOT NULL UNIQUE,
    size_bytes    INTEGER NOT NULL,
    properties    TEXT NOT NULL,
    imported_at   TEXT NOT NULL
);
CREATE INDEX media_files_by_asset ON media_files (asset_id);

-- Originals are immutable: their bytes, checksum and location never change.
CREATE TRIGGER media_files_immutable BEFORE UPDATE ON media_files
BEGIN
    SELECT RAISE(ABORT, 'media files are immutable');
END;

CREATE TABLE variants (
    id              TEXT PRIMARY KEY NOT NULL,
    asset_id        TEXT NOT NULL REFERENCES assets (id),
    media_file_id   TEXT NOT NULL REFERENCES media_files (id),
    name            TEXT NOT NULL,
    recipe          TEXT NOT NULL,
    recipe_revision INTEGER NOT NULL,
    render          TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
CREATE INDEX variants_by_asset ON variants (asset_id);

CREATE TABLE accounts (
    id             TEXT PRIMARY KEY NOT NULL,
    platform       TEXT NOT NULL,
    display_name   TEXT NOT NULL,
    capabilities   TEXT NOT NULL,
    -- A key into the OS credential store; never a secret.
    credential_ref TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

CREATE TABLE publications (
    id               TEXT PRIMARY KEY NOT NULL,
    asset_id         TEXT NOT NULL REFERENCES assets (id),
    platform         TEXT NOT NULL,
    state            TEXT NOT NULL CHECK (state IN (
        'draft', 'awaiting_approval', 'scheduled', 'preparing', 'uploading', 'processing',
        'publishing', 'published', 'retry_scheduled', 'needs_authentication',
        'needs_user_action', 'outcome_unknown', 'failed', 'cancelled')),
    current_revision INTEGER NOT NULL,
    -- Copied from the current revision so they can be queried.
    account_id       TEXT NOT NULL REFERENCES accounts (id),
    variant_id       TEXT NOT NULL REFERENCES variants (id),
    approval         TEXT,
    schedule_at      TEXT,
    schedule_tz      TEXT,
    retry_at         TEXT,
    user_action      TEXT,
    last_error       TEXT,
    published        TEXT,
    version          INTEGER NOT NULL,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    FOREIGN KEY (id, current_revision) REFERENCES publication_revisions (publication_id, number)
        DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX publications_by_asset ON publications (asset_id);
CREATE INDEX publications_by_account ON publications (account_id, schedule_at);
CREATE INDEX publications_by_variant ON publications (variant_id);

CREATE TABLE publication_revisions (
    publication_id TEXT NOT NULL REFERENCES publications (id) DEFERRABLE INITIALLY DEFERRED,
    number         INTEGER NOT NULL,
    content        TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    PRIMARY KEY (publication_id, number)
);

-- Approvals bind to revisions, so revisions must never change or disappear.
CREATE TRIGGER publication_revisions_immutable BEFORE UPDATE ON publication_revisions
BEGIN
    SELECT RAISE(ABORT, 'publication revisions are immutable');
END;
CREATE TRIGGER publication_revisions_permanent BEFORE DELETE ON publication_revisions
BEGIN
    SELECT RAISE(ABORT, 'publication revisions are permanent');
END;

CREATE TABLE publication_events (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    publication_id TEXT NOT NULL REFERENCES publications (id),
    from_state     TEXT,
    to_state       TEXT NOT NULL,
    event          TEXT NOT NULL,
    revision       INTEGER NOT NULL,
    at             TEXT NOT NULL
);
CREATE INDEX publication_events_by_publication ON publication_events (publication_id, id);

CREATE TABLE jobs (
    id               TEXT PRIMARY KEY NOT NULL,
    kind             TEXT NOT NULL,
    subject          TEXT NOT NULL,
    payload          TEXT NOT NULL,
    state            TEXT NOT NULL CHECK (state IN (
        'queued', 'running', 'paused', 'succeeded', 'failed', 'cancelled', 'expired')),
    run_at           TEXT NOT NULL,
    time_sensitive   INTEGER NOT NULL,
    claims           INTEGER NOT NULL,
    failures         INTEGER NOT NULL,
    max_failures     INTEGER NOT NULL,
    lease_owner      TEXT,
    lease_expires_at TEXT,
    heartbeat_at     TEXT,
    checkpoint       TEXT,
    last_error       TEXT,
    pause_reason     TEXT,
    version          INTEGER NOT NULL,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);
CREATE INDEX jobs_due ON jobs (state, run_at);
CREATE INDEX jobs_by_lease ON jobs (state, lease_expires_at);
-- One open job per subject keeps a publication from having two execution paths.
CREATE UNIQUE INDEX jobs_one_open_per_subject ON jobs (kind, subject)
    WHERE state IN ('queued', 'running', 'paused');

CREATE TABLE publish_attempts (
    id                   TEXT PRIMARY KEY NOT NULL,
    publication_id       TEXT NOT NULL REFERENCES publications (id),
    revision             INTEGER NOT NULL,
    job_id               TEXT REFERENCES jobs (id),
    stage                TEXT NOT NULL,
    upload_session       TEXT,
    remote_media_id      TEXT,
    remote_post_id       TEXT,
    publish_requested_at TEXT,
    outcome              TEXT NOT NULL,
    started_at           TEXT NOT NULL,
    finished_at          TEXT
);
CREATE INDEX publish_attempts_by_publication ON publish_attempts (publication_id, started_at);

CREATE TABLE settings (
    key        TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
