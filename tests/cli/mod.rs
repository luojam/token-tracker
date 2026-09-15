use crate::support::{TempTree, jsonl, records};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output};

const ALL_USAGE: &str = include_str!("../fixtures/pi/all-usage.jsonl");
const CODEX_USAGE: &str = include_str!("../fixtures/codex/response-mirrors.jsonl");
const CLAUDE_USAGE: &str = include_str!("../fixtures/claude/snapshots.jsonl");
const CLAUDE_SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";

fn command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_token-tracker"));
    command
        .env("HOME", home)
        .env_remove("PI_CODING_AGENT_SESSION_DIR")
        .env_remove("PI_CODING_AGENT_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("HERMES_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CONFIG_HOME");
    command
}

fn run_command(session_root: &Path, data_home: &Path, home: &Path) -> Output {
    command(home)
        .env("PI_CODING_AGENT_SESSION_DIR", session_root)
        .env("XDG_DATA_HOME", data_home)
        .output()
        .unwrap()
}

fn successful_report(output: Output) -> String {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());

    let report = String::from_utf8(output.stdout).unwrap();
    assert!(!report.contains("SECRET_"));
    report
}

fn assert_totals(report: &str, tokens: [u64; 4], sessions: u64, events: u64) {
    for (label, value) in [
        ("Input tokens", tokens[0]),
        ("Output tokens", tokens[1]),
        ("Cache-read tokens", tokens[2]),
        ("Cache-write tokens", tokens[3]),
        ("Total tokens", tokens.iter().sum()),
        ("Sessions", sessions),
        ("Unique usage events", events),
    ] {
        assert!(report.contains(&format!("{label}: {value}\n")), "{report}");
    }
}

fn append(path: &Path, content: &str) {
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(content.as_bytes())
        .unwrap();
}

fn assert_no_content_persisted(database_directory: &Path) {
    assert!(database_directory.join("usage.db").is_file());

    // Raw bytes catch content left in freed pages and SQLite sidecars.
    for entry in fs::read_dir(database_directory).unwrap() {
        let path = entry.unwrap().path();
        let bytes = fs::read(&path).unwrap();
        assert!(
            !bytes
                .windows(b"SECRET_".len())
                .any(|bytes| bytes == b"SECRET_"),
            "conversation content persisted in {}",
            path.display()
        );
    }
}

#[test]
fn imports_pi_codex_and_claude_and_preserves_usage_privately_across_runs() {
    let tree = TempTree::new();
    let home = tree.root.join("home");
    let data_home = tree.root.join("data");
    let config = home.join(".claude");
    let codex_home = tree.root.join("custom-codex");
    let project = config.join("projects/invented-project");
    let source = project.join(format!("{CLAUDE_SESSION_ID}.jsonl"));
    let claude = CLAUDE_USAGE
        .replace(
            "\"type\":\"text\"",
            "\"type\":\"text\",\"text\":\"SECRET_CLAUDE_RESPONSE\"",
        )
        .replace(
            "\"content\":[]",
            "\"content\":[{\"type\":\"tool_result\",\"content\":\"SECRET_CLAUDE_TOOL\"}]",
        );

    let codex = format!(
        "{CODEX_USAGE}{}",
        concat!(
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"content\":\"SECRET_CODEX_MESSAGE\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"output\":\"SECRET_CODEX_TOOL\"}}\n",
        )
    );
    for (path, content) in [
        (home.join(".pi/agent/sessions/history.jsonl"), ALL_USAGE),
        (
            codex_home.join("sessions/rollout-history.jsonl"),
            codex.as_str(),
        ),
        (source.clone(), claude.as_str()),
    ] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    let run = || {
        successful_report(
            command(&home)
                .env("XDG_DATA_HOME", &data_home)
                .env("CODEX_HOME", &codex_home)
                .output()
                .unwrap(),
        )
    };

    let report = run();
    assert_totals(&report, [163, 113, 341, 144], 3, 8);
    assert!(
        report.contains("Total cost: $1.026345 (partial)\n"),
        "{report}"
    );
    assert!(report.contains("Claude Code usage:"), "{report}");
    assert!(report.contains("anthropic / claude-opus-5"), "{report}");
    assert!(!report.contains("Warnings"), "{report}");
    assert_eq!(run(), report);

    append(
        &source,
        include_str!("../fixtures/claude/partial-ignored.jsonl"),
    );
    fs::write(
        project.join("22222222-2222-4222-8222-222222222222.jsonl"),
        "{SECRET_CLAUDE_MALFORMED}\n",
    )
    .unwrap();

    let partial = run();
    assert_totals(&partial, [165, 116, 341, 144], 3, 9);
    for warning in [
        "Warnings (2):\n",
        "Claude: 2 responses across 1 file excluded because final usage is missing.",
        "malformed Claude session line 1",
    ] {
        assert!(partial.contains(warning), "{partial}");
    }
    assert_eq!(partial.matches("final usage is missing.").count(), 1);
    assert_eq!(run(), partial);
    assert_no_content_persisted(&data_home.join("token-tracker"));

    let unavailable = successful_report(
        command(&home)
            .env_remove("HOME")
            .env("XDG_DATA_HOME", &data_home)
            .output()
            .unwrap(),
    );
    assert!(unavailable.contains("claude: could not configure adapter:"));
    assert_eq!(unavailable.matches("final usage is missing.").count(), 1);
    assert_totals(&unavailable, [165, 116, 341, 144], 3, 9);

    fs::remove_dir_all(&config).unwrap();
    fs::remove_dir_all(home.join(".pi")).unwrap();
    fs::remove_dir_all(&codex_home).unwrap();

    let retained = run();
    assert_eq!(
        retained.split_once("\nWarnings").unwrap().0,
        partial.split_once("\nWarnings").unwrap().0
    );
    assert!(
        retained
            .contains("Claude: 2 responses across 1 file excluded because final usage is missing.")
    );
}

#[test]
fn legacy_request_pricing_survives_reopen_corrections_and_source_removal() {
    use serde_json::{Value, json};

    let tree = TempTree::new();
    let home = tree.root.join("home");
    let sessions = home.join(".codex/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let source = sessions.join("rollout-legacy.jsonl");
    let mut records = records(include_str!("../fixtures/codex/legacy-fresh.jsonl"));
    records[2]["payload"]["model"] = json!("gpt-5.5");
    for record in &mut records {
        if !record["payload"]["info"].is_object() {
            continue;
        }
        for vector in ["total_token_usage", "last_token_usage"] {
            if let Some(counters) = record["payload"]["info"][vector].as_object_mut() {
                for counter in counters.values_mut() {
                    *counter = json!(counter.as_u64().unwrap() * 2_000);
                }
            }
        }
    }

    let write = |records: &[Value]| {
        fs::write(&source, jsonl(records)).unwrap();
    };
    let run = || successful_report(command(&home).output().unwrap());
    write(&records);

    let report = run();
    assert!(report.contains("Total cost: $3.100000\n"), "{report}");
    assert_totals(
        &report.replace(',', ""),
        [240_000, 60_000, 200_000, 0],
        1,
        1,
    );
    assert_eq!(run(), report);

    records[6]["payload"]["info"]["last_token_usage"] =
        records[6]["payload"]["info"]["total_token_usage"].clone();
    records.drain(4..6);
    write(&records);

    let corrected = run();
    assert!(corrected.contains("Total cost: $5.300000\n"), "{corrected}");
    assert_totals(
        &corrected.replace(',', ""),
        [240_000, 60_000, 200_000, 0],
        1,
        1,
    );
    assert_eq!(run(), corrected);

    fs::remove_file(source).unwrap();
    assert_eq!(run(), corrected);
}

#[test]
fn storage_failure_exits_without_a_report() {
    let tree = TempTree::new();
    let sessions = tree.root.join("sessions");
    let data_home = tree.root.join("data");
    let home = tree.root.join("home");
    fs::create_dir(&sessions).unwrap();
    fs::create_dir(&data_home).unwrap();
    fs::create_dir(&home).unwrap();
    fs::write(data_home.join("token-tracker"), "not a directory").unwrap();

    let output = run_command(&sessions, &data_home, &home);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("token-tracker: could not open usage storage:")
    );
}

#[test]
fn export_round_trips_retained_usage_without_refreshing() {
    use token_tracker::{TokenTracker, TokenTrackerConfig};

    let tree = TempTree::new();
    let source = tree.write(".pi/agent/sessions/history.jsonl", ALL_USAGE);
    tree.write(".codex/sessions/rollout-history.jsonl", CODEX_USAGE);
    successful_report(command(&tree.root).output().unwrap());
    let tracker = TokenTracker::open(TokenTrackerConfig {
        database_path: Some(tree.root.join(".local/share/token-tracker/usage.db")),
        sources: vec![],
        ..Default::default()
    })
    .unwrap();
    let expected = tracker.export_snapshot().unwrap();
    fs::write(source, "malformed source must not be refreshed\n").unwrap();

    let path = tree.root.join("export.db");
    let output = command(&tree.root)
        .arg("export")
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    let connection = rusqlite::Connection::open(&path).unwrap();
    let snapshot = read_export(&connection);
    assert_eq!(snapshot.machine_name, None);
    assert_eq!(snapshot.events, expected.events);
    assert_eq!(snapshot.machine_id, expected.machine_id);
    assert_eq!(snapshot.export_revision, expected.export_revision + 1);
    assert_eq!(snapshot.format_version, expected.format_version);
    assert!(!String::from_utf8_lossy(&fs::read(path).unwrap()).contains("SECRET_"));
}

#[test]
fn config_sets_export_name_with_xdg_precedence_and_home_fallback() {
    let tree = TempTree::new();
    tree.write(
        ".config/token-tracker/config.toml",
        "machine_name = 'laptop'\n",
    );
    tree.write(
        "config/token-tracker/config.toml",
        "machine_name = 'work'\n",
    );
    let path = tree.root.join("export.db");
    let run = |config_home: &Path| {
        let output = command(&tree.root)
            .env("XDG_CONFIG_HOME", config_home)
            .arg("export")
            .arg(&path)
            .arg("--force")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        read_export(&rusqlite::Connection::open(&path).unwrap())
    };

    let home = run(Path::new("relative-config"));
    assert_eq!(home.machine_name.as_deref(), Some("laptop"));
    let xdg = run(&tree.root.join("config"));
    assert_eq!(xdg.machine_name.as_deref(), Some("work"));
    assert_eq!(xdg.machine_id, home.machine_id);
}

#[test]
fn invalid_config_fails_before_opening_storage_but_help_still_works() {
    let tree = TempTree::new();
    for content in [
        "machine_name = [",
        "machine_name = 42",
        "machine_nam = 'typo'",
    ] {
        let path = tree.write(".config/token-tracker/config.toml", content);
        let output = command(&tree.root).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("could not load config"), "{error}");
        assert!(error.contains(path.to_str().unwrap()), "{error}");
    }
    assert!(!tree.root.join(".local").exists());
    assert!(
        command(&tree.root)
            .arg("--help")
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn export_replaces_both_tables_atomically_and_requires_force() {
    let tree = TempTree::new();
    tree.write(".pi/agent/sessions/history.jsonl", ALL_USAGE);
    successful_report(command(&tree.root).output().unwrap());
    let path = tree.root.join("export.db");
    let run = |force: bool| {
        let mut command = command(&tree.root);
        command.arg("export").arg(&path);
        if force {
            command.arg("--force");
        }
        command.output().unwrap()
    };
    assert!(run(false).status.success());
    let connection = rusqlite::Connection::open(&path).unwrap();
    let original = read_export(&connection);
    assert!(!original.events.is_empty());
    let refused = run(false);
    assert!(!refused.status.success());
    assert!(refused.stdout.is_empty());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("use --force to overwrite"));
    assert_eq!(read_export(&connection), original);

    connection
        .execute_batch(
            "CREATE TRIGGER fail_export BEFORE INSERT ON events
         BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;",
        )
        .unwrap();
    let failed = run(true);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("injected write failure"));
    assert_eq!(read_export(&connection), original);
    connection
        .execute_batch("DROP TRIGGER fail_export")
        .unwrap();

    fs::remove_file(tree.root.join(".local/share/token-tracker/usage.db")).unwrap();
    let forced = run(true);
    assert!(forced.status.success(), "{forced:?}");
    let replaced = read_export(&connection);
    assert!(replaced.events.is_empty());
    assert_eq!(replaced.machine_id, original.machine_id);
    assert!(replaced.export_revision > original.export_revision);

    let unrelated = tree.write("unrelated.db", "keep this file\n");
    for destination in [unrelated.clone(), tree.root.join("missing/export.db")] {
        let failed = command(&tree.root)
            .arg("export")
            .arg(destination)
            .arg("--force")
            .output()
            .unwrap();
        assert!(!failed.status.success());
        assert!(failed.stdout.is_empty());
        assert!(String::from_utf8_lossy(&failed.stderr).contains("could not write export to"));
    }
    assert_eq!(fs::read_to_string(unrelated).unwrap(), "keep this file\n");
    let storage = tree.root.join("unrelated-sqlite.db");
    fs::copy(
        tree.root.join(".local/share/token-tracker/usage.db"),
        &storage,
    )
    .unwrap();
    let before = fs::read(&storage).unwrap();
    let failed = command(&tree.root)
        .arg("export")
        .arg(&storage)
        .arg("--force")
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("not a token-tracker export database")
    );
    assert!(fs::read(storage).unwrap() == before);
}

fn read_export(connection: &rusqlite::Connection) -> token_tracker::ExportSnapshot {
    let json: String = connection
        .query_row(
            "SELECT json_object(
            'machine_id', machine_id, 'machine_name', machine_name,
            'export_revision', json(export_revision), 'format_version', format_version,
            'exported_at_unix_ms', exported_at_unix_ms,
            'events', (SELECT json_group_array(json_object(
                'agent', agent, 'event_key', event_key, 'timestamp_unix_ms', timestamp_unix_ms,
                'usage_kind', usage_kind, 'provider', provider, 'model', model,
                'tokens', json_object('input', json(input_tokens), 'output', json(output_tokens),
                    'cache_read', json(cache_read_tokens), 'cache_write', json(cache_write_tokens)),
                'recorded_cost_usd', recorded_cost_usd, 'estimate', json(estimate),
                'pricing_context', json(pricing_context), 'sessions', json(sessions)
            )) FROM (SELECT * FROM events ORDER BY rowid))
        ) FROM snapshot",
            [],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&json).unwrap()
}

#[test]
fn invalid_export_arguments_fail_before_opening_storage() {
    let tree = TempTree::new();
    for args in [
        vec!["export"],
        vec!["export", "export.db", "--unknown"],
        vec!["export", "one.db", "two.db"],
    ] {
        let output = command(&tree.root).args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
    }
    assert!(!tree.root.join(".local").exists());
}

#[test]
fn adapter_setup_failure_still_reports_stored_usage() {
    let tree = TempTree::new();
    let home = tree.root.join("home");
    let sessions = tree.root.join("sessions");
    let data_home = tree.root.join("data");
    fs::create_dir(&sessions).unwrap();
    fs::write(sessions.join("history.jsonl"), ALL_USAGE).unwrap();

    assert_eq!(
        successful_report(run_command(&sessions, &data_home, &home)),
        include_str!("../fixtures/all_time_report.txt")
    );

    let report = successful_report(
        command(&home)
            .env_remove("HOME")
            .env("XDG_DATA_HOME", &data_home)
            .output()
            .unwrap(),
    );
    assert_totals(&report, [25, 38, 51, 64], 1, 4);
    assert!(report.contains("pi: could not configure adapter:"));
    assert!(report.contains("codex: could not configure adapter:"));
    assert!(report.contains("claude: could not configure adapter:"));
}

#[cfg(target_os = "linux")]
#[test]
fn output_failure_exits_with_an_error() {
    let tree = TempTree::new();
    let output = command(&tree.root)
        .stdout(OpenOptions::new().write(true).open("/dev/full").unwrap())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .starts_with("token-tracker: could not write report:")
    );
}

#[test]
fn hermes_discovers_default_and_profile_databases_and_respects_hermes_home() {
    let tree = TempTree::new();
    let home = tree.root.join("home");
    for (relative, id) in [
        (".hermes/state.db", "default"),
        (".hermes/profiles/work/state.db", "profile"),
        (".hermes/backups/state.db", "backup"),
        (".hermes/profiles/work/nested/state.db", "nested"),
        ("custom/state.db", "custom"),
        ("custom/profiles/ignored/state.db", "ignored"),
    ] {
        let path = home.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        crate::support::hermes::database(&path)
            .execute_batch(&format!(
                "DELETE FROM session_model_usage; DELETE FROM sessions;
                 INSERT INTO sessions (id, started_at) VALUES ('{id}', 1700000000);"
            ))
            .unwrap();
    }

    let run = |name: &str, hermes_home: &str| {
        successful_report(
            command(&home)
                .current_dir(&home)
                .env("XDG_DATA_HOME", tree.root.join(name))
                .env("HERMES_HOME", hermes_home)
                .output()
                .unwrap(),
        )
    };

    let default = run("default-data", "");
    assert!(default.contains("Sessions: 2\n"), "{default}");
    assert!(!default.contains("Warnings"), "{default}");

    let custom = run("custom-data", "custom");
    assert!(custom.contains("Sessions: 1\n"), "{custom}");
}

#[test]
fn imports_hermes_and_keeps_other_agents_working_with_a_broken_source() {
    let tree = TempTree::new();
    let home = tree.root.join("home");
    tree.write("home/.pi/agent/sessions/history.jsonl", ALL_USAGE);
    let path = home.join(".hermes/state.db");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = crate::support::hermes::database(&path);

    let report = successful_report(command(&home).output().unwrap());
    assert_totals(&report.replace(',', ""), [197, 98, 761, 96], 3, 9);
    for expected in ["Hermes usage:", "Pi usage:", "openai / model-a"] {
        assert!(report.contains(expected), "{report}");
    }

    connection
        .execute_batch("ALTER TABLE session_model_usage RENAME COLUMN task TO legacy_task;")
        .unwrap();

    let fresh = successful_report(
        command(&home)
            .env("XDG_DATA_HOME", tree.root.join("fresh-data"))
            .output()
            .unwrap(),
    );
    assert_totals(&fresh, [25, 38, 51, 64], 1, 4);
    assert!(fresh.contains("Pi usage:"), "{fresh}");
    assert!(fresh.contains("unsupported Hermes schema"), "{fresh}");
}
