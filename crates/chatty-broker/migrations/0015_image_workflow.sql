-- Optional API-format workflow template; empty keeps the built-in workflow.
ALTER TABLE broker_settings ADD COLUMN image_workflow TEXT NOT NULL DEFAULT '';
