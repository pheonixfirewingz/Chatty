CREATE TABLE worlds (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    data BLOB NOT NULL,
    revision INTEGER NOT NULL
);
CREATE INDEX idx_worlds_owner ON worlds(owner_id, revision);
