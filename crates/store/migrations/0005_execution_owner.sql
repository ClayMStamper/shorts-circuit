-- Execution ownership (cloud mode). Every existing publication is desktop-owned. While the cloud
-- runner owns one, `handover_id` names the hand-over that gave it to the runner.

ALTER TABLE publications ADD COLUMN owner TEXT NOT NULL DEFAULT 'desktop'
    CHECK (owner IN ('desktop', 'cloud'));
ALTER TABLE publications ADD COLUMN handover_id TEXT;
CREATE INDEX publications_by_owner ON publications (owner);
