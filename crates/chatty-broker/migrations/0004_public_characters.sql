ALTER TABLE characters ADD COLUMN is_public INTEGER NOT NULL DEFAULT 0;
CREATE INDEX idx_characters_public ON characters(is_public, name) WHERE is_public = 1;
