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
    pricing_tier TEXT
        CHECK (pricing_tier IN ('standard', 'fast', 'unknown', 'unsupported')),
    pricing_unsupported_tier TEXT,
    pricing_raw_tier_kind TEXT
        CHECK (pricing_raw_tier_kind IN ('missing', 'null', 'value')),
    pricing_raw_tier_value TEXT,
    pricing_tier_evidence TEXT
        CHECK (pricing_tier_evidence IN ('unknown', 'requested_setting', 'served_response')),
    pricing_request_granularity TEXT
        CHECK (pricing_request_granularity IN ('exact_single_request', 'aggregate_or_unknown')),
    pricing_cache_detail TEXT
        CHECK (pricing_cache_detail IN ('complete', 'incomplete')),
    pricing_request_usage BLOB
        CHECK (pricing_request_usage IS NULL OR
               (pricing_tier IS NOT NULL AND typeof(pricing_request_usage) = 'blob'
                AND length(pricing_request_usage) > 0 AND length(pricing_request_usage) % 32 = 0)),
    CHECK (
        (pricing_tier IS NULL AND pricing_unsupported_tier IS NULL
         AND pricing_raw_tier_kind IS NULL AND pricing_raw_tier_value IS NULL
         AND pricing_tier_evidence IS NULL AND pricing_request_granularity IS NULL
         AND pricing_cache_detail IS NULL)
        OR
        (pricing_tier IS NOT NULL AND pricing_raw_tier_kind IS NOT NULL
         AND pricing_tier_evidence IS NOT NULL AND pricing_request_granularity IS NOT NULL
         AND pricing_cache_detail IS NOT NULL
         AND (pricing_tier IS 'unsupported') = (pricing_unsupported_tier IS NOT NULL)
         AND (pricing_raw_tier_kind IS 'value') = (pricing_raw_tier_value IS NOT NULL))
    ),
    PRIMARY KEY (source_session_id, event_id),
    FOREIGN KEY (source_session_id, source_id) REFERENCES source_sessions(id, source_id),
    CHECK ((provider IS NULL) = (model IS NULL)),
    CHECK (recorded_cost_usd IS NULL OR recorded_cost_usd >= 0.0)
);

CREATE INDEX source_sessions_identity ON source_sessions(agent, session_id);
CREATE INDEX source_observations_event ON source_observations(event_id);

PRAGMA user_version = 1;
