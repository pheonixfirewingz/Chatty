-- Hidden reasoning effort for chat replies: default leaves the choice to the
-- model and server; off, low, medium, and high are sent to the provider.
ALTER TABLE broker_settings ADD COLUMN reasoning TEXT NOT NULL DEFAULT 'default'
    CHECK (reasoning IN ('default', 'off', 'low', 'medium', 'high'));
