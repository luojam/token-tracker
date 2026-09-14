use std::path::Path;

use rusqlite::Connection;

pub fn database(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY, started_at REAL NOT NULL,
            input_tokens INTEGER DEFAULT 0, output_tokens INTEGER DEFAULT 0,
            cache_read_tokens INTEGER DEFAULT 0, cache_write_tokens INTEGER DEFAULT 0,
            reasoning_tokens INTEGER DEFAULT 0, api_call_count INTEGER DEFAULT 0,
            cwd TEXT, title TEXT, parent_session_id TEXT,
            model_config BLOB, hidden INTEGER DEFAULT 0, archived INTEGER DEFAULT 0
        );
        CREATE TABLE session_model_usage (
            session_id TEXT NOT NULL, model TEXT NOT NULL,
            billing_provider TEXT NOT NULL DEFAULT '', billing_base_url TEXT NOT NULL DEFAULT '',
            billing_mode TEXT NOT NULL DEFAULT '', task TEXT NOT NULL DEFAULT '',
            input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens INTEGER NOT NULL DEFAULT 0, cache_write_tokens INTEGER NOT NULL DEFAULT 0,
            reasoning_tokens INTEGER NOT NULL DEFAULT 0, api_call_count INTEGER NOT NULL DEFAULT 0,
            first_seen REAL, last_seen REAL,
            actual_cost_usd REAL DEFAULT 0, estimated_cost_usd REAL DEFAULT 0,
            cost_status TEXT, cost_source TEXT, extra_column BLOB,
            PRIMARY KEY (session_id, model, billing_provider, billing_base_url, billing_mode, task)
        );
        INSERT INTO sessions (id, started_at, input_tokens, output_tokens,
            cache_read_tokens, cache_write_tokens, reasoning_tokens, api_call_count,
            cwd, title, parent_session_id, hidden, archived)
        VALUES ('child', 1700000000.125, 160, 55, 600, 30, 45, 12,
            '/workspace', 'Synthetic session', 'parent', 1, 1);
        INSERT INTO sessions (id, started_at, cwd) VALUES ('empty', 1700000001, 'relative');
        INSERT INTO session_model_usage (session_id, model, billing_provider, billing_mode,
            task, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
            reasoning_tokens, api_call_count, first_seen)
        VALUES
            ('child', 'model-a', 'openai-codex', 'subscription', '', 100, 20, 400, 10, 15, 5, NULL),
            ('child', 'model-b', 'openai-codex', 'subscription', '', 40, 35, 250, 20, 30, 7, 1700000002.5),
            ('child', 'model-a', 'auto', '', 'compression', 5, 3, 50, 2, 2, 1, 1700000003),
            ('child', 'model-a', 'auto', '', 'title_generation', 7, 2, 10, 0, 1, 1, 1700000004);"
    ).unwrap();
    connection
}
