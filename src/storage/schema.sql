CREATE TABLE import_sources (
    id INTEGER PRIMARY KEY,
    source_key BLOB NOT NULL,
    path BLOB,
    agent TEXT NOT NULL,
    last_observed_revision BLOB NOT NULL,
    last_imported_revision BLOB,
    last_discovery_scan_ms INTEGER NOT NULL,
    last_successful_scan_ms INTEGER,
    last_parse_completion TEXT CHECK (last_parse_completion IN ('complete', 'partial')),
    normalization_version INTEGER CHECK (normalization_version BETWEEN 1 AND 4294967295),
    parse_notices TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(parse_notices)),
    present INTEGER NOT NULL CHECK (present IN (0, 1)),
    UNIQUE (agent, source_key),
    CHECK (
        (last_imported_revision IS NULL AND last_successful_scan_ms IS NULL
         AND last_parse_completion IS NULL AND normalization_version IS NULL
         AND parse_notices = '[]')
        OR
        (last_imported_revision IS NOT NULL AND last_successful_scan_ms IS NOT NULL
         AND last_parse_completion IS NOT NULL AND normalization_version IS NOT NULL)
    )
);

CREATE TABLE usage_events (
    id INTEGER PRIMARY KEY,
    agent TEXT NOT NULL,
    adapter_key TEXT NOT NULL,
    UNIQUE (agent, adapter_key)
);

CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    source_id INTEGER NOT NULL REFERENCES import_sources(id),
    agent TEXT NOT NULL,
    session_id TEXT NOT NULL,
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

CREATE TABLE usage_observations (
    source_id INTEGER NOT NULL REFERENCES import_sources(id),
    source_session_id INTEGER NOT NULL,
    event_id INTEGER NOT NULL REFERENCES usage_events(id),
    timestamp_ms INTEGER NOT NULL,
    usage_kind TEXT NOT NULL,
    provider TEXT,
    model TEXT,
    input_tokens INTEGER NOT NULL CHECK (input_tokens >= 0),
    output_tokens INTEGER NOT NULL CHECK (output_tokens >= 0),
    cache_read_tokens INTEGER NOT NULL CHECK (cache_read_tokens >= 0),
    cache_write_tokens INTEGER NOT NULL CHECK (cache_write_tokens >= 0),
    recorded_cost_usd REAL,
    PRIMARY KEY (source_session_id, event_id),
    FOREIGN KEY (source_session_id, source_id) REFERENCES sessions(id, source_id),
    CHECK ((provider IS NULL) = (model IS NULL)),
    CHECK (recorded_cost_usd IS NULL OR recorded_cost_usd >= 0.0)
);

CREATE INDEX sessions_identity ON sessions(agent, session_id);
CREATE INDEX usage_observations_event ON usage_observations(event_id);

CREATE TABLE billing_inputs (
    source_session_id INTEGER NOT NULL,
    event_id INTEGER NOT NULL,
    facts TEXT NOT NULL CHECK (json_valid(facts)),
    PRIMARY KEY (source_session_id, event_id),
    FOREIGN KEY (source_session_id, event_id)
        REFERENCES usage_observations(source_session_id, event_id)
);
