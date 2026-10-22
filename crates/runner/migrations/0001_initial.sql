-- Shorts Circuit cloud runner schema, version 1. Instants are timestamptz (UTC). JSONB columns
-- hold serde-serialized domain values; the columns next to them are copies for querying.

-- A desktop installation allowed to use this runner. Only the SHA-256 of its token is kept.
CREATE TABLE installations (
    id           UUID PRIMARY KEY,
    name         TEXT NOT NULL,
    token_sha256 TEXT NOT NULL UNIQUE,
    created_at   TIMESTAMPTZ NOT NULL,
    last_seen_at TIMESTAMPTZ,
    revoked_at   TIMESTAMPTZ
);

CREATE TABLE accounts (
    id              UUID PRIMARY KEY,
    installation_id UUID NOT NULL REFERENCES installations (id),
    platform        TEXT NOT NULL,
    record          JSONB NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL
);

-- Envelope-encrypted: `ciphertext` is the credential JSON sealed with a per-write data key,
-- which is stored wrapped by the master key `key_id`. Neither key is stored here in the clear.
CREATE TABLE credentials (
    account_id  UUID PRIMARY KEY REFERENCES accounts (id) ON DELETE CASCADE,
    revision    BIGINT NOT NULL,
    updated_by  TEXT NOT NULL CHECK (updated_by IN ('desktop', 'runner')),
    key_id      TEXT NOT NULL,
    wrapped_key BYTEA NOT NULL,
    ciphertext  BYTEA NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL
);

-- Resumable, content-addressed media uploads (the bytes live on disk).
CREATE TABLE uploads (
    installation_id UUID NOT NULL REFERENCES installations (id),
    sha256          TEXT NOT NULL,
    size_bytes      BIGINT NOT NULL CHECK (size_bytes >= 0),
    received_bytes  BIGINT NOT NULL CHECK (received_bytes >= 0 AND received_bytes <= size_bytes),
    complete        BOOLEAN NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (installation_id, sha256)
);

CREATE TABLE publications (
    id              UUID PRIMARY KEY,
    installation_id UUID NOT NULL REFERENCES installations (id),
    account_id      UUID NOT NULL REFERENCES accounts (id),
    handover_id     UUID NOT NULL,
    owner           TEXT NOT NULL CHECK (owner IN ('desktop', 'cloud')),
    state           TEXT NOT NULL,
    revision        INTEGER NOT NULL,
    record          JSONB NOT NULL,
    media           JSONB NOT NULL,
    media_sha256    TEXT NOT NULL,
    version         BIGINT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL
);
CREATE INDEX publications_by_installation ON publications (installation_id, updated_at);
CREATE INDEX publications_by_account ON publications (account_id, state);
CREATE INDEX publications_by_media ON publications (media_sha256);

-- The revisions handed over. Approvals bind to them, so they never change or disappear.
CREATE TABLE publication_revisions (
    publication_id UUID NOT NULL REFERENCES publications (id),
    number         INTEGER NOT NULL,
    content        JSONB NOT NULL,
    received_at    TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (publication_id, number)
);

CREATE FUNCTION publication_revisions_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'publication revisions are immutable';
END;
$$;
CREATE TRIGGER publication_revisions_immutable BEFORE UPDATE OR DELETE ON publication_revisions
    FOR EACH ROW EXECUTE FUNCTION publication_revisions_immutable();

CREATE TABLE publication_events (
    id             BIGSERIAL PRIMARY KEY,
    publication_id UUID NOT NULL REFERENCES publications (id),
    from_state     TEXT,
    to_state       TEXT NOT NULL,
    event          TEXT NOT NULL,
    revision       INTEGER NOT NULL,
    at             TIMESTAMPTZ NOT NULL
);
CREATE INDEX publication_events_by_publication ON publication_events (publication_id, id);

CREATE TABLE jobs (
    id               UUID PRIMARY KEY,
    kind             TEXT NOT NULL,
    subject          TEXT NOT NULL,
    payload          JSONB NOT NULL,
    state            TEXT NOT NULL CHECK (state IN (
        'queued', 'running', 'paused', 'succeeded', 'failed', 'cancelled', 'expired')),
    run_at           TIMESTAMPTZ NOT NULL,
    time_sensitive   BOOLEAN NOT NULL,
    claims           INTEGER NOT NULL,
    failures         INTEGER NOT NULL,
    max_failures     INTEGER NOT NULL,
    lease_owner      TEXT,
    lease_expires_at TIMESTAMPTZ,
    heartbeat_at     TIMESTAMPTZ,
    checkpoint       JSONB,
    last_error       TEXT,
    pause_reason     TEXT,
    version          BIGINT NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL,
    updated_at       TIMESTAMPTZ NOT NULL
);
CREATE INDEX jobs_due ON jobs (state, run_at);
CREATE INDEX jobs_by_subject ON jobs (kind, subject, created_at);
-- One open job per subject keeps a publication from having two execution paths.
CREATE UNIQUE INDEX jobs_one_open_per_subject ON jobs (kind, subject)
    WHERE state IN ('queued', 'running', 'paused');

CREATE TABLE attempts (
    id             UUID PRIMARY KEY,
    publication_id UUID NOT NULL REFERENCES publications (id),
    record         JSONB NOT NULL,
    updated_at     TIMESTAMPTZ NOT NULL
);
CREATE INDEX attempts_by_publication ON attempts (publication_id);

-- Provider quota spent per day (YouTube: the Pacific-time date).
CREATE TABLE quota_usage (
    bucket     TEXT NOT NULL,
    day        DATE NOT NULL,
    units      INTEGER NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (bucket, day)
);
