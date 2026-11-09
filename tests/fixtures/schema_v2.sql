-- Relevant columns from T3 Code 0.1.7's v2 orchestrator projections (statev2.sqlite, migration 79).
CREATE TABLE orchestration_v2_projection_threads (
    thread_id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    deleted_at TEXT
);
CREATE TABLE orchestration_v2_projection_runs (
    run_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    provider TEXT NOT NULL,
    status TEXT NOT NULL,
    payload_json TEXT NOT NULL
);
CREATE TABLE orchestration_v2_projection_run_attempts (
    attempt_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    status TEXT NOT NULL
);
CREATE TABLE orchestration_v2_projection_provider_turns (
    provider_turn_id TEXT PRIMARY KEY,
    run_attempt_id TEXT,
    status TEXT NOT NULL,
    started_at TEXT
);
CREATE TABLE orchestration_v2_projection_nodes (
    node_id TEXT PRIMARY KEY,
    run_id TEXT
);
CREATE TABLE orchestration_v2_projection_runtime_requests (
    runtime_request_id TEXT PRIMARY KEY,
    node_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    status TEXT NOT NULL
);
CREATE TABLE effect_sql_migrations (
    migration_id INTEGER PRIMARY KEY NOT NULL,
    name TEXT NOT NULL
);
