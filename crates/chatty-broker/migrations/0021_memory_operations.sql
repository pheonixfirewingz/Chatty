-- Structured memory operations (ADR 0007).
-- status: 0 active, 1 superseded, 2 retired. A superseded row points at its successor.
ALTER TABLE memories ADD COLUMN superseded_by TEXT REFERENCES memories(id) ON DELETE SET NULL;
-- Consecutive invalid model outputs at the current checkpoint.
ALTER TABLE memory_extraction_state ADD COLUMN failures INTEGER NOT NULL DEFAULT 0;
CREATE INDEX idx_memory_history ON memories(owner_id, character_id, status, updated_at);
