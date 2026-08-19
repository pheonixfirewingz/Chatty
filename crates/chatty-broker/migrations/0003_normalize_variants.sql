-- Regenerations belong in variants, not as duplicate timeline messages.
INSERT OR IGNORE INTO variants(id, message_id, content, created_at, revision)
SELECT id, parent_id, content, created_at, revision
FROM messages
WHERE parent_id IS NOT NULL;

DELETE FROM messages WHERE parent_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_variants_message ON variants(message_id, created_at, id);
