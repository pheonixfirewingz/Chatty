-- Path to the in-process emotion classifier GGUF; empty disables reply-time
-- portrait classification.
ALTER TABLE broker_settings ADD COLUMN emotion_model TEXT NOT NULL DEFAULT '';
