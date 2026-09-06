CREATE TABLE sources (
    id INTEGER PRIMARY KEY,
    path BLOB NOT NULL,
    agent TEXT NOT NULL,
    session_id TEXT,
    format_version TEXT,
    working_directory BLOB,
    started_at_ms INTEGER,
    name TEXT,
    parent_session BLOB,
    parent_kind TEXT,
    last_observed_size BLOB NOT NULL,
    last_observed_modified_seconds INTEGER NOT NULL,
    last_observed_modified_nanos INTEGER NOT NULL,
    last_imported_size BLOB,
    last_imported_modified_seconds INTEGER,
    last_imported_modified_nanos INTEGER,
    last_discovery_scan_ms INTEGER NOT NULL,
    last_successful_scan_ms INTEGER,
    last_parse_completion TEXT,
    present INTEGER NOT NULL CHECK (present IN (0, 1)),
    UNIQUE (agent, path),
    CHECK (last_observed_modified_nanos BETWEEN 0 AND 999999999),
    CHECK (
        (last_imported_size IS NULL AND last_imported_modified_seconds IS NULL
         AND last_imported_modified_nanos IS NULL)
        OR
        (last_imported_size IS NOT NULL AND last_imported_modified_seconds IS NOT NULL
         AND last_imported_modified_nanos BETWEEN 0 AND 999999999)
    ),
    CHECK ((parent_session IS NULL) = (parent_kind IS NULL)),
    CHECK (parent_kind IN ('source_path', 'session_id'))
);

CREATE TABLE usage_events (
    id INTEGER PRIMARY KEY,
    agent TEXT NOT NULL,
    adapter_key TEXT NOT NULL,
    UNIQUE (agent, adapter_key)
);

CREATE TABLE source_sessions (
    id INTEGER PRIMARY KEY,
    source_id INTEGER NOT NULL REFERENCES sources(id),
    agent TEXT NOT NULL,
    session_id TEXT NOT NULL,
    format_version TEXT,
    working_directory BLOB,
    started_at_ms INTEGER NOT NULL,
    name TEXT,
    parent_session BLOB,
    parent_kind TEXT,
    UNIQUE (source_id, agent, session_id),
    UNIQUE (id, source_id),
    CHECK ((parent_session IS NULL) = (parent_kind IS NULL)),
    CHECK (parent_kind IN ('source_path', 'session_id'))
);

CREATE TABLE source_observations (
    source_id INTEGER NOT NULL REFERENCES sources(id),
    source_session_id INTEGER NOT NULL,
    event_id INTEGER NOT NULL REFERENCES usage_events(id),
    timestamp_ms INTEGER NOT NULL,
    usage_kind TEXT NOT NULL,
    provider TEXT,
    model TEXT,
    input_tokens BLOB NOT NULL,
    output_tokens BLOB NOT NULL,
    cache_read_tokens BLOB NOT NULL,
    cache_write_tokens BLOB NOT NULL,
    recorded_cost_usd REAL,
    PRIMARY KEY (source_session_id, event_id),
    FOREIGN KEY (source_session_id, source_id) REFERENCES source_sessions(id, source_id),
    CHECK ((provider IS NULL) = (model IS NULL)),
    CHECK (recorded_cost_usd IS NULL OR recorded_cost_usd >= 0.0)
);

CREATE INDEX source_sessions_identity ON source_sessions(agent, session_id);
CREATE INDEX source_observations_event ON source_observations(event_id);

PRAGMA user_version = 1;
