PRAGMA foreign_keys = ON;

CREATE TABLE users (id TEXT PRIMARY KEY, username TEXT NOT NULL UNIQUE, password_hash TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'user', created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE sessions (token TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE, expires_at TEXT NOT NULL DEFAULT (datetime('now','+30 days')));
CREATE TABLE characters (id TEXT PRIMARY KEY, owner_id TEXT NOT NULL REFERENCES users(id), name TEXT NOT NULL, personality TEXT NOT NULL, scenario TEXT NOT NULL, system_prompt TEXT NOT NULL, example_dialogue TEXT NOT NULL, appearance TEXT NOT NULL, tags BLOB NOT NULL, avatar BLOB, revision INTEGER NOT NULL);
CREATE INDEX idx_characters_owner ON characters(owner_id, revision);
CREATE TABLE conversations (id TEXT PRIMARY KEY, owner_id TEXT NOT NULL REFERENCES users(id), title TEXT NOT NULL, kind INTEGER NOT NULL, state BLOB NOT NULL DEFAULT X'', turn_index INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL);
CREATE INDEX idx_conversations_owner ON conversations(owner_id, revision);
CREATE TABLE participants (conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE, character_id TEXT NOT NULL REFERENCES characters(id), position INTEGER NOT NULL, PRIMARY KEY(conversation_id, character_id));
CREATE TABLE messages (id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE, author_type TEXT NOT NULL, author_id TEXT, content TEXT NOT NULL, parent_id TEXT REFERENCES messages(id), selected_variant_id TEXT, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, revision INTEGER NOT NULL);
CREATE INDEX idx_messages_context ON messages(conversation_id, created_at DESC);
CREATE TABLE variants (id TEXT PRIMARY KEY, message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE, content TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, revision INTEGER NOT NULL);
CREATE TABLE lore (id TEXT PRIMARY KEY, owner_id TEXT NOT NULL REFERENCES users(id), conversation_id TEXT REFERENCES conversations(id), keywords BLOB NOT NULL, content TEXT NOT NULL, always_on INTEGER NOT NULL, priority INTEGER NOT NULL, revision INTEGER NOT NULL);
CREATE INDEX idx_lore_scope ON lore(owner_id, conversation_id, priority DESC);
CREATE TABLE memories (id TEXT PRIMARY KEY, owner_id TEXT NOT NULL REFERENCES users(id), conversation_id TEXT REFERENCES conversations(id), character_id TEXT REFERENCES characters(id), content TEXT NOT NULL, revision INTEGER NOT NULL);
CREATE INDEX idx_memory_scope ON memories(owner_id, conversation_id, character_id);
CREATE TABLE deltas (revision INTEGER PRIMARY KEY AUTOINCREMENT, owner_id TEXT NOT NULL, entity_type TEXT NOT NULL, entity_id TEXT NOT NULL, operation INTEGER NOT NULL, changed_fields BLOB NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE INDEX idx_deltas_sync ON deltas(owner_id, revision);
