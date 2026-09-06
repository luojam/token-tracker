use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const ALL_USAGE: &str = include_str!("fixtures/pi/all-usage.jsonl");
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
    // not just the currently queryable rows. All fixture content has this marker.
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
    assert!(report.contains("Recorded cost: $1.020000\n"));
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
    assert!(retained.contains("Recorded cost: $1.020000\n"));

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
        assert!(report.contains("Recorded cost: $1.020000\n"));
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
        assert!(updated.contains("Recorded cost: $1.020000\n"));
        fs::remove_file(&parent_path).unwrap();
        assert_eq!(run(), updated);
        assert_no_content_persisted(&data_home.join("token-tracker"));
        reports.push((report, updated));
    }
    assert_eq!(reports[0], reports[1]);
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
