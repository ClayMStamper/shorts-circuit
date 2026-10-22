-- Rules count only assets entering at or after this instant (and with no older source post).
-- Added here rather than in 0002, which some libraries had already applied before the column
-- existed; rules from then start counting from when they were created.
ALTER TABLE collection_rules ADD COLUMN effective_from TEXT NOT NULL DEFAULT '';
UPDATE collection_rules SET effective_from = created_at WHERE effective_from = '';
