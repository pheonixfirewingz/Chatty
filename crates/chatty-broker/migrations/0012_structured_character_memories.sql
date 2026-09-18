ALTER TABLE memories ADD COLUMN kind INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memories ADD COLUMN importance INTEGER NOT NULL DEFAULT 50;
ALTER TABLE memories ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memories ADD COLUMN confidence REAL NOT NULL DEFAULT 1.0;
ALTER TABLE memories ADD COLUMN source INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memories ADD COLUMN source_message_ids TEXT NOT NULL DEFAULT '[]';
ALTER TABLE memories ADD COLUMN status INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memories ADD COLUMN created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP;
ALTER TABLE memories ADD COLUMN updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP;
ALTER TABLE memories ADD COLUMN last_accessed_at TEXT;
ALTER TABLE memories ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;

CREATE INDEX idx_memory_retrieval
    ON memories(owner_id, character_id, conversation_id, status, pinned, importance DESC);

CREATE VIRTUAL TABLE memory_fts USING fts5(memory_id UNINDEXED, content);

INSERT INTO memory_fts(memory_id, content)
    SELECT id, content FROM memories;

CREATE TRIGGER memories_fts_insert AFTER INSERT ON memories BEGIN
    INSERT INTO memory_fts(memory_id, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER memories_fts_update AFTER UPDATE OF content ON memories BEGIN
    DELETE FROM memory_fts WHERE memory_id = old.id;
    INSERT INTO memory_fts(memory_id, content) VALUES (new.id, new.content);
END;

CREATE TRIGGER memories_fts_delete AFTER DELETE ON memories BEGIN
    DELETE FROM memory_fts WHERE memory_id = old.id;
END;

CREATE TABLE memory_extraction_state (
    owner_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    last_message_revision INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(owner_id, conversation_id)
);
