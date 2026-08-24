ALTER TABLE users ADD COLUMN prompt_tokens INTEGER NOT NULL DEFAULT 0 CHECK (prompt_tokens >= 0);
ALTER TABLE users ADD COLUMN completion_tokens INTEGER NOT NULL DEFAULT 0 CHECK (completion_tokens >= 0);
