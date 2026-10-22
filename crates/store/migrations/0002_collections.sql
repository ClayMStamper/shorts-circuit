-- Collections, collection rules, unattended-publishing grants and the rule firing log.

CREATE TABLE collections (
    id         TEXT PRIMARY KEY NOT NULL,
    name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE collection_members (
    collection_id TEXT NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    asset_id      TEXT NOT NULL REFERENCES assets (id) ON DELETE CASCADE,
    added_at      TEXT NOT NULL,
    PRIMARY KEY (collection_id, asset_id)
);
CREATE INDEX collection_members_by_asset ON collection_members (asset_id);

CREATE TABLE collection_rules (
    id            TEXT PRIMARY KEY NOT NULL,
    collection_id TEXT NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    -- Serialized RuleSpec: name, enabled, destinations, output, caption template, hashtags.
    spec          TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE INDEX collection_rules_by_collection ON collection_rules (collection_id);

-- Unattended publishing is off unless a row exists for the collection and account.
CREATE TABLE unattended_grants (
    collection_id TEXT NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    account_id    TEXT NOT NULL REFERENCES accounts (id),
    enabled_at    TEXT NOT NULL,
    enabled_by    TEXT NOT NULL,
    PRIMARY KEY (collection_id, account_id)
);

-- One row per rule, asset and destination: a rule never creates a second publication for the
-- same asset and account, even after a re-import or after the rule is deleted and the asset
-- re-added. rule_id has no foreign key so the history outlives deleted rules.
CREATE TABLE rule_firings (
    rule_id          TEXT NOT NULL,
    asset_id         TEXT NOT NULL REFERENCES assets (id),
    account_id       TEXT NOT NULL REFERENCES accounts (id),
    publication_id   TEXT NOT NULL REFERENCES publications (id),
    unattended       INTEGER NOT NULL,
    auto_approved_at TEXT,
    fired_at         TEXT NOT NULL,
    PRIMARY KEY (rule_id, asset_id, account_id)
);
CREATE INDEX rule_firings_by_asset ON rule_firings (asset_id);
