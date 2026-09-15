CREATE TABLE IF NOT EXISTS snapshot (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    machine_id TEXT NOT NULL,
    machine_name TEXT,
    export_revision TEXT NOT NULL,
    format_version INTEGER NOT NULL,
    exported_at_unix_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
    agent TEXT NOT NULL,
    event_key TEXT NOT NULL,
    timestamp_unix_ms INTEGER NOT NULL,
    usage_kind TEXT NOT NULL,
    provider TEXT,
    model TEXT,
    input_tokens TEXT NOT NULL,
    output_tokens TEXT NOT NULL,
    cache_read_tokens TEXT NOT NULL,
    cache_write_tokens TEXT NOT NULL,
    recorded_cost_usd TEXT,
    estimate TEXT NOT NULL CHECK (json_valid(estimate)),
    pricing_context TEXT CHECK (json_valid(pricing_context)),
    sessions TEXT NOT NULL CHECK (json_valid(sessions)),
    PRIMARY KEY (agent, event_key)
);
