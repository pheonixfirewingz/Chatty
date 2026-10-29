-- Optional small model for automatic memory extraction. Empty uses the chat model.
ALTER TABLE broker_settings ADD COLUMN memory_model TEXT NOT NULL DEFAULT '';
