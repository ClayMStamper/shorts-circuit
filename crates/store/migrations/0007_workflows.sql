-- Workflows: "source -> one destination account". Each wraps one collection rule (name, enabled,
-- caption template, hashtags, effective_from live there) and adds the owner's rights
-- confirmation and a few settings. Auto Publish is the unattended grant for the rule's collection
-- and the account, so there is at most one workflow per collection and account.
CREATE TABLE workflows (
    id                  TEXT PRIMARY KEY NOT NULL,
    rule_id             TEXT NOT NULL UNIQUE REFERENCES collection_rules (id) ON DELETE CASCADE,
    collection_id       TEXT NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    account_id          TEXT NOT NULL REFERENCES accounts (id),
    -- Serialized WorkflowSettings: posting mode, destination options.
    settings            TEXT NOT NULL,
    rights_confirmed_at TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    UNIQUE (collection_id, account_id)
);
