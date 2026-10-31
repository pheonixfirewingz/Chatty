-- Timeline reads order by seq, so the created_at index has no readers.
DROP INDEX idx_messages_context;
-- One position per message in each conversation timeline.
DROP INDEX idx_messages_timeline;
CREATE UNIQUE INDEX idx_messages_timeline ON messages(conversation_id, seq);
-- Memory extraction finds rows changed since its checkpoint by revision.
CREATE INDEX idx_messages_revision ON messages(conversation_id, revision);
