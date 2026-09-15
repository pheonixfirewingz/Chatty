ALTER TABLE characters ADD COLUMN images BLOB NOT NULL DEFAULT X'';
ALTER TABLE characters ADD COLUMN default_image_id TEXT;
ALTER TABLE messages ADD COLUMN character_image_id TEXT;
