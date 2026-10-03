-- Relevant columns from T3 Code 0.0.45's persisted projections.
CREATE TABLE projection_threads (
    thread_id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    deleted_at TEXT,
    pending_approval_count INTEGER NOT NULL DEFAULT 0,
    pending_user_input_count INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE projection_thread_sessions (
    thread_id TEXT PRIMARY KEY,
    status TEXT NOT NULL,
    active_turn_id TEXT
);
CREATE TABLE provider_session_runtime (
    thread_id TEXT PRIMARY KEY,
    provider_name TEXT NOT NULL,
    status TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    runtime_payload_json TEXT
);
CREATE TABLE projection_turns (
    row_id INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id TEXT NOT NULL,
    turn_id TEXT,
    state TEXT NOT NULL,
    -- The reader's join relies on this to report each turn once.
    UNIQUE (thread_id, turn_id)
);
CREATE TABLE effect_sql_migrations (
    migration_id INTEGER PRIMARY KEY NOT NULL,
    name TEXT NOT NULL
);
