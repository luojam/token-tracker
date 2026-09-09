use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const ALL_USAGE: &str = include_str!("fixtures/pi/all-usage.jsonl");
const CODEX_USAGE: &str = include_str!("fixtures/codex/response-mirrors.jsonl");
static NEXT_TEMP_TREE: AtomicU64 = AtomicU64::new(0);

struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new() -> Self {
        let sequence = NEXT_TEMP_TREE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "token-tracker-cli-test-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        Self { root }
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_token-tracker"));
    command
        .env("HOME", home)
        .env_remove("PI_CODING_AGENT_SESSION_DIR")
        .env_remove("PI_CODING_AGENT_DIR")
        .env_remove("CODEX_HOME")
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
    // Check raw database pages (including freed pages) and any SQLite sidecars,
    // not just the currently queryable rows. Fixture conversation content uses SECRET_.
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

fn header(session_id: &str) -> String {
    format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"{session_id}\",\"timestamp\":\"2025-01-02T03:04:05.000Z\",\"cwd\":\"/work/project\"}}\n"
    )
}

#[test]
fn command_preserves_history_and_privacy_through_the_session_lifecycle() {
    let tree = TempTree::new();
    let home = tree.root.join("home");
    let sessions = home.join(".pi/agent/sessions");
    let project = sessions.join("project-a");
    let other_project = sessions.join("project-b");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir(&other_project).unwrap();
    let path = project.join("history.jsonl");
    fs::write(&path, ALL_USAGE).unwrap();
    fs::write(
        other_project.join("malformed.jsonl"),
        format!("{}{{SECRET_MALFORMED_CONTENT}}\n", header("bad-session")),
    )
    .unwrap();

    // No overrides: exercise both default session discovery and HOME storage.
    let run = || successful_report(command(&home).output().unwrap());
    let report = run();
    assert_totals(&report, [25, 38, 51, 64], 1, 4);
    assert!(report.contains("Total cost: $1.020000\n"));
    for group in [
        "provider-a / model-resolved",
        "Unattributed tool results",
        "Unattributed compactions",
        "Unattributed branch summaries",
    ] {
        assert!(report.contains(group), "{report}");
    }
    assert!(report.contains("Warnings (1):\n"));
    assert!(report.contains("malformed Pi session line 2"));
    assert_eq!(run(), report);
    let database_directory = home.join(".local/share/token-tracker");
    assert_no_content_persisted(&database_directory);

    let event = |id: &str, input: u64| {
        format!(
            "{{\"type\":\"message\",\"id\":\"{id}\",\"timestamp\":\"2025-01-02T04:00:00.000Z\",\"message\":{{\"role\":\"assistant\",\"provider\":\"provider-a\",\"model\":\"model-resolved\",\"content\":\"SECRET_APPENDED_RESPONSE\",\"usage\":{{\"input\":{input},\"output\":2,\"cacheRead\":3,\"cacheWrite\":4}}}}}}\n"
        )
    };
    let pending = event("pending-event", 8);
    let (prefix, suffix) = pending.split_at(pending.len() / 2);
    append(&path, &format!("{}{prefix}", event("appended-event", 7)));
    let incomplete = run();
    assert_totals(&incomplete, [32, 40, 54, 68], 1, 5);
    assert_eq!(run(), incomplete);
    append(&path, suffix);
    let completed = run();
    assert_totals(&completed, [40, 42, 57, 72], 1, 6);
    assert_eq!(run(), completed);

    // Update one observation while removing all the other usage from the file.
    // Their incurred usage must remain in the database and the report.
    let rewritten = ALL_USAGE
        .lines()
        .take(3)
        .collect::<Vec<_>>()
        .join("\n")
        .replacen(
            "\"input\":10,\"output\":20",
            "\"input\":100,\"output\":20",
            1,
        );
    fs::write(&path, format!("{rewritten}\n")).unwrap();
    let retained = run();
    assert_totals(&retained, [130, 42, 57, 72], 1, 6);
    assert!(retained.contains("Total cost: $1.020000\n"));

    // Even valid new usage before a malformed complete line must not be committed.
    append(&path, &event("must-not-commit", 99));
    append(&path, "{SECRET_MALFORMED_REWRITE}\n");
    let malformed = run();
    assert_totals(&malformed, [130, 42, 57, 72], 1, 6);
    assert!(malformed.contains("Warnings (2):\n"));
    assert!(malformed.contains("malformed Pi session line 5"));
    fs::remove_file(&path).unwrap();
    assert_eq!(run(), retained);
    assert_eq!(run(), retained);
    assert_no_content_persisted(&database_directory);
}

#[test]
fn command_reconciles_conflicting_forks_in_either_import_order() {
    let mut reports = Vec::new();
    for parent_first in [true, false] {
        let tree = TempTree::new();
        let sessions = tree.root.join("sessions");
        let data_home = tree.root.join("data");
        let home = tree.root.join("home");
        fs::create_dir(&sessions).unwrap();
        fs::create_dir(&home).unwrap();
        let parent_path = sessions.join("parent.jsonl");
        let child_path = sessions.join("child.jsonl");
        let (parent_header, entries) = ALL_USAGE.split_once('\n').unwrap();
        let mut child_header: serde_json::Value = serde_json::from_str(parent_header).unwrap();
        child_header["id"] = "child-session".into();
        child_header["parentSession"] = parent_path.to_str().unwrap().into();
        // Ancestry must win even if the child's recorded start time is earlier.
        child_header["timestamp"] = "2025-01-01T00:00:00.000Z".into();
        let child = format!("{child_header}\n{entries}")
            .replacen(
                "\"input\":10,\"output\":20",
                "\"input\":999,\"output\":20",
                1,
            )
            .replacen("\"total\":0.12", "\"total\":9.99", 1);
        let run = || successful_report(run_command(&sessions, &data_home, &home));
        let sources = [(&parent_path, ALL_USAGE), (&child_path, child.as_str())];
        let order = if parent_first { [0, 1] } else { [1, 0] };
        for index in order {
            fs::write(sources[index].0, sources[index].1).unwrap();
            run();
        }

        let report = run();
        assert_totals(&report, [25, 38, 51, 64], 2, 4);
        assert!(report.contains("Total cost: $1.020000\n"));
        assert!(!report.contains("Warnings"));
        assert_eq!(run(), report);

        // A newer descendant import cannot replace the ancestor's observation.
        fs::write(
            &child_path,
            child.replacen("\"input\":999", "\"input\":9999", 1),
        )
        .unwrap();
        assert_eq!(run(), report);
        fs::write(
            &parent_path,
            ALL_USAGE.replacen(
                "\"input\":10,\"output\":20",
                "\"input\":100,\"output\":20",
                1,
            ),
        )
        .unwrap();
        let updated = run();
        assert_totals(&updated, [115, 38, 51, 64], 2, 4);
        assert!(updated.contains("Total cost: $1.020000\n"));
        fs::remove_file(&parent_path).unwrap();
        assert_eq!(run(), updated);
        assert_no_content_persisted(&data_home.join("token-tracker"));
        reports.push((report, updated));
    }
    assert_eq!(reports[0], reports[1]);
}

#[test]
fn command_reports_both_adapters_with_missing_or_failing_roots() {
    for (pi_present, codex_present, failing_agent) in [
        (true, true, None),
        (false, true, None),
        (true, false, None),
        (false, true, Some("pi")),
        (true, false, Some("codex")),
    ] {
        let tree = TempTree::new();
        let home = tree.root.join("home");
        let pi_root = home.join(".pi/agent/sessions");
        let codex_root = home.join(".codex/sessions");
        for (agent, root, present, filename, content) in [
            ("pi", &pi_root, pi_present, "history.jsonl", ALL_USAGE),
            (
                "codex",
                &codex_root,
                codex_present,
                "rollout-history.jsonl",
                CODEX_USAGE,
            ),
        ] {
            fs::create_dir_all(root.parent().unwrap()).unwrap();
            if present {
                fs::create_dir(root).unwrap();
                fs::write(root.join(filename), content).unwrap();
            } else if failing_agent == Some(agent) {
                fs::write(root, "not a directory").unwrap();
            }
        }

        let run = || successful_report(command(&home).output().unwrap());
        let report = run();
        let pi = u64::from(pi_present);
        let codex = u64::from(codex_present);
        assert_totals(
            &report,
            [
                25 * pi + 120 * codex,
                38 * pi + 30 * codex,
                51 * pi + 100 * codex,
                64 * pi,
            ],
            pi + codex,
            4 * pi + 2 * codex,
        );
        let cost = match (pi_present, codex_present) {
            (true, true) => Some("$1.024460"),
            (true, false) => Some("$1.020000"),
            (false, true) => Some("$0.004460"),
            (false, false) => None,
        };
        if let Some(cost) = cost {
            assert!(
                report.contains(&format!("Total cost: {cost}\n")),
                "{report}"
            );
        } else {
            assert!(!report.contains("Total cost:"));
        }
        assert!(!report.contains("API-equivalent estimate (Codex):"));
        if codex_present {
            assert!(
                report.lines().any(|line| {
                    line.starts_with("  openai / gpt-6-astra ") && line.ends_with("$0.004460")
                }),
                "{report}"
            );
            assert!(!report.contains(" (partial)"));
        }
        if let Some(agent) = failing_agent {
            assert!(report.contains("Warnings (1):\n"), "{report}");
            assert!(
                report.contains(&format!("{agent}: could not read directory:")),
                "{report}"
            );
        } else {
            assert!(!report.contains("Warnings"), "{report}");
        }
        assert_eq!(run(), report);
    }
}

#[test]
fn command_preserves_mixed_usage_and_estimates_through_codex_lifecycle() {
    let mut reports = Vec::new();
    for parent_first in [true, false] {
        let tree = TempTree::new();
        let home = tree.root.join("home");
        let pi_root = home.join(".pi/agent/sessions");
        let codex_home = tree.root.join("custom-codex");
        let sessions = codex_home.join("sessions/2026/01");
        let archive = codex_home.join("archived_sessions");
        fs::create_dir_all(&pi_root).unwrap();
        fs::create_dir_all(&sessions).unwrap();
        fs::create_dir(&archive).unwrap();
        fs::write(pi_root.join("history.jsonl"), ALL_USAGE).unwrap();

        let lines = CODEX_USAGE.split_inclusive('\n').collect::<Vec<_>>();
        let standard = lines[6].replace("priority", "default");
        let content = concat!(
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"content\":\"SECRET_CODEX_MESSAGE\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call_output\",\"output\":\"SECRET_CODEX_TOOL\"}}\n",
        );
        let response = format!("{}{standard}{content}{}", lines[0], lines[1..].concat());
        let response_path = sessions.join("rollout-response.jsonl");
        let cut = response.find(lines[9]).unwrap() + lines[9].len() / 2;
        fs::write(&response_path, &response[..cut]).unwrap();

        // Every invocation reopens the same on-disk ledger.
        let run = || {
            successful_report(
                command(&home)
                    .env("CODEX_HOME", &codex_home)
                    .output()
                    .unwrap(),
            )
        };
        let partial = run();
        assert_totals(&partial, [85, 48, 91, 64], 2, 5);
        assert!(partial.contains("Total cost: $1.021140\n"));
        assert_eq!(run(), partial);

        let sources = [
            (
                sessions.join("rollout-parent.jsonl"),
                include_str!("fixtures/codex/legacy-parent.jsonl"),
            ),
            (
                sessions.join("rollout-fork.jsonl"),
                include_str!("fixtures/codex/legacy-fork.jsonl"),
            ),
        ];
        for index in if parent_first { [0, 1] } else { [1, 0] } {
            fs::write(&sources[index].0, sources[index].1).unwrap();
            run();
        }
        fs::write(
            sessions.join("rollout-subagent.jsonl"),
            include_str!("fixtures/codex/response-subagent.jsonl"),
        )
        .unwrap();
        let inherited = run();
        assert_totals(&inherited, [325, 98, 271, 64], 5, 8);

        append(&response_path, &response[cut..]);
        let completed = run();
        assert_totals(&completed, [385, 118, 331, 64], 5, 9);
        assert!(completed.contains("Total cost: $1.029540\n"), "{completed}");
        assert!(
            completed.lines().any(|line| {
                line.starts_with("  openai / gpt-6-astra ") && line.ends_with("$0.009540")
            }),
            "{completed}"
        );
        assert!(!completed.contains("Warnings"), "{completed}");

        let archived_response = archive.join("rollout-renamed.jsonl");
        fs::copy(&response_path, &archived_response).unwrap();
        assert_eq!(run(), completed);
        fs::remove_file(&response_path).unwrap();
        fs::rename(&sources[0].0, archive.join("rollout-parent.jsonl")).unwrap();
        assert_eq!(run(), completed);

        let database_directory = home.join(".local/share/token-tracker");
        assert_no_content_persisted(&database_directory);
        fs::write(
            &archived_response,
            format!(
                "{}{{SECRET_CODEX_MALFORMED_REWRITE}}\n",
                response.replace("priority", "default")
            ),
        )
        .unwrap();
        let malformed = run();
        assert!(malformed.contains("Warnings (1):\n"), "{malformed}");
        assert!(
            malformed.contains("malformed Codex session line"),
            "{malformed}"
        );
        assert_eq!(malformed.split_once("\nWarnings").unwrap().0, completed);

        fs::remove_dir_all(&codex_home).unwrap();
        fs::remove_dir_all(&pi_root).unwrap();
        assert_eq!(run(), completed);
        assert_no_content_persisted(&database_directory);
        reports.push(completed);
    }
    assert_eq!(reports[0], reports[1]);
}

#[test]
fn legacy_request_pricing_survives_reopen_corrections_and_source_removal() {
    use serde_json::{Value, json};

    let tree = TempTree::new();
    let home = tree.root.join("home");
    let sessions = home.join(".codex/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let source = sessions.join("rollout-legacy.jsonl");
    let mut records: Vec<Value> = include_str!("fixtures/codex/legacy-fresh.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
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
        fs::write(
            &source,
            records.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
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
fn legacy_pricing_keeps_assumptions_through_cached_runs() {
    use serde_json::{Value, json};

    for (model, tier, cache_complete, expected) in [
        ("gpt-5.6-sol", Some("default"), true, "$0.001120"),
        ("gpt-5.6-sol", Some("priority"), true, "$0.002240"),
        ("gpt-5.6-sol", None, true, "$0.001120"),
        ("gpt-5.6-sol", Some("default"), false, "$0.001120"),
        ("gpt-5.6-terra", Some("default"), true, "$0.000620"),
        ("gpt-5.6-terra", Some("priority"), true, "$0.001240"),
        ("gpt-5.6-terra", None, false, "$0.000620"),
        ("gpt-5.6-luna", Some("default"), true, "$0.000062"),
        ("gpt-5.6-luna", Some("fast"), true, "$0.000124"),
        ("gpt-5.6-luna", None, false, "$0.000062"),
        ("gpt-5.5", Some("default"), false, "$0.001550"),
        ("gpt-5.5", Some("priority"), false, "$0.003875"),
        ("gpt-5.5", None, false, "$0.001550"),
        ("gpt-5.4-mini", Some("priority"), false, "$0.000465"),
        ("gpt-5.4-mini", None, false, "$0.000233"),
        ("gpt-5.4-mini", Some("auto"), false, "$0.000233"),
        ("codex-auto-review", Some("priority"), true, "unavailable"),
    ] {
        let tree = TempTree::new();
        let home = tree.root.join("home");
        let sessions = home.join(".codex/sessions");
        fs::create_dir_all(&sessions).unwrap();
        let mut records: Vec<Value> = include_str!("fixtures/codex/legacy-fresh.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        records[2]["payload"]["model"] = json!(model);
        for record in &mut records {
            let info = &mut record["payload"]["info"];
            if cache_complete && info.is_object() {
                info["total_token_usage"]["cache_write_input_tokens"] = json!(0);
                info["last_token_usage"]["cache_write_input_tokens"] = json!(0);
            }
        }
        if let Some(tier) = tier {
            records.insert(1, json!({
                "timestamp": "2026-01-01T00:00:00Z", "type": "event_msg",
                "payload": {"type": "thread_settings_applied", "thread_settings": {"service_tier": tier}}
            }));
        }
        let source = sessions.join("rollout-legacy.jsonl");
        fs::write(
            &source,
            records.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
        let run = || successful_report(command(&home).output().unwrap());
        let report = run();
        assert!(
            report.contains(&format!("Total cost: {expected}\n")),
            "{model}: {report}"
        );
        assert_totals(&report, [120, 30, 100, 0], 1, 1);
        assert!(
            report.lines().any(|line| {
                line.starts_with(&format!("  openai / {model} ")) && line.ends_with(expected)
            }),
            "{report}"
        );
        assert_eq!(run(), report);
        fs::remove_file(source).unwrap();
        assert_eq!(run(), report);
    }
}

#[test]
fn command_returns_failure_when_default_storage_cannot_be_opened() {
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
fn command_reports_stored_usage_when_adapter_setup_is_unavailable() {
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
}
