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
        .env_remove("XDG_DATA_HOME");
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
fn imports_all_adapters_and_preserves_usage_privately_across_runs() {
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
        "omitted 2 responses with incomplete usage",
        "malformed Claude session line 1",
    ] {
        assert!(partial.contains(warning), "{partial}");
    }
    assert_eq!(run(), partial);
    assert_no_content_persisted(&data_home.join("token-tracker"));
    fs::remove_dir_all(&config).unwrap();
    fs::remove_dir_all(home.join(".pi")).unwrap();
    fs::remove_dir_all(&codex_home).unwrap();
    let retained = run();
    assert_eq!(
        retained.split_once("\nWarnings").unwrap().0,
        partial.split_once("\nWarnings").unwrap().0
    );
    assert!(retained.contains("omitted 2 responses with incomplete usage"));
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
fn adapter_setup_failure_still_reports_stored_usage() {
    let tree = TempTree::new();
    let home = tree.root.join("home");
    let sessions = tree.root.join("sessions");
    let data_home = tree.root.join("data");
    fs::create_dir(&sessions).unwrap();
    fs::write(sessions.join("history.jsonl"), ALL_USAGE).unwrap();
    successful_report(run_command(&sessions, &data_home, &home));

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
