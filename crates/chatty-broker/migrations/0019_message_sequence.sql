-- Stable per-conversation timeline order. created_at has one-second resolution
-- and the implicit rowid may be renumbered by VACUUM; revision changes on edit.
ALTER TABLE messages ADD COLUMN seq INTEGER;
UPDATE messages SET seq = ordered.n
FROM (
    SELECT rowid AS row_id,
           ROW_NUMBER() OVER (PARTITION BY conversation_id ORDER BY created_at, rowid) AS n
    FROM messages
) AS ordered
WHERE messages.rowid = ordered.row_id;
CREATE INDEX idx_messages_timeline ON messages(conversation_id, seq);
-- Number every new message after the newest in its conversation, whatever the insert path.
CREATE TRIGGER messages_assign_seq AFTER INSERT ON messages
WHEN NEW.seq IS NULL
BEGIN
    UPDATE messages
    SET seq = (SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE conversation_id = NEW.conversation_id)
    WHERE rowid = NEW.rowid;
END;
