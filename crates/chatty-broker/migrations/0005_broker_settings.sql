CREATE TABLE broker_settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    adapter_enabled INTEGER NOT NULL DEFAULT 1,
    adapter_url TEXT NOT NULL DEFAULT 'http://192.168.0.97:11434/v1',
    allow_public_characters INTEGER NOT NULL DEFAULT 1,
    allow_self_registration INTEGER NOT NULL DEFAULT 1,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
