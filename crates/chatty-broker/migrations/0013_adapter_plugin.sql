-- Empty preserves the existing use_ollama_api selection.
ALTER TABLE broker_settings ADD COLUMN adapter_plugin TEXT NOT NULL DEFAULT '';
