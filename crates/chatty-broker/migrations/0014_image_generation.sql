-- Text-to-image service settings; disabled until an administrator configures it.
ALTER TABLE broker_settings ADD COLUMN image_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE broker_settings ADD COLUMN image_url TEXT NOT NULL DEFAULT 'http://127.0.0.1:8188';
ALTER TABLE broker_settings ADD COLUMN image_plugin TEXT NOT NULL DEFAULT 'comfyui';
ALTER TABLE broker_settings ADD COLUMN image_model TEXT NOT NULL DEFAULT '';
ALTER TABLE broker_settings ADD COLUMN image_width INTEGER NOT NULL DEFAULT 1024;
ALTER TABLE broker_settings ADD COLUMN image_height INTEGER NOT NULL DEFAULT 1024;
ALTER TABLE broker_settings ADD COLUMN image_steps INTEGER NOT NULL DEFAULT 25;
ALTER TABLE broker_settings ADD COLUMN image_guidance REAL NOT NULL DEFAULT 7.0;
ALTER TABLE broker_settings ADD COLUMN image_negative_prompt TEXT NOT NULL DEFAULT '';
-- Encrypted PNG for messages with author_type 'image'.
CREATE TABLE message_images (
    message_id TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    data BLOB NOT NULL
);
