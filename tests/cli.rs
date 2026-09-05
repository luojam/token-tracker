use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

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

fn run_command(session_root: &Path, data_home: &Path, home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_token-tracker"))
        .env("PI_CODING_AGENT_SESSION_DIR", session_root)
        .env("XDG_DATA_HOME", data_home)
        .env("HOME", home)
        .output()
        .unwrap()
}

fn header(session_id: &str) -> String {
    format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"{session_id}\",\"timestamp\":\"2025-01-02T03:04:05.000Z\",\"cwd\":\"/work/project\"}}\n"
    )
}

#[test]
fn command_imports_sessions_reports_warnings_and_remains_idempotent() {
    let tree = TempTree::new();
    let sessions = tree.root.join("sessions");
    let data_home = tree.root.join("data");
    let home = tree.root.join("home");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir(&home).unwrap();

    fs::write(
        sessions.join("valid.jsonl"),
        format!(
            "{}{{\"type\":\"message\",\"id\":\"event-a\",\"timestamp\":\"2025-01-02T03:05:00.000Z\",\"message\":{{\"role\":\"assistant\",\"provider\":\"provider\",\"model\":\"model\",\"usage\":{{\"input\":10,\"output\":2,\"cacheRead\":3,\"cacheWrite\":4}}}}}}\n",
            header("valid-session")
        ),
    )
    .unwrap();
    fs::write(
        sessions.join("malformed.jsonl"),
        format!("{}{{malformed}}\n", header("bad-session")),
    )
    .unwrap();

    let first = run_command(&sessions, &data_home, &home);
    assert!(first.status.success());
    assert!(first.stderr.is_empty());
    let report = String::from_utf8(first.stdout).unwrap();
    assert!(report.contains("Input tokens: 10\n"));
    assert!(report.contains("Total tokens: 19\n"));
    assert!(report.contains("Sessions: 1\n"));
    assert!(report.contains("Unique usage events: 1\n"));
    assert!(report.contains("Warnings (1):\n"));
    assert!(report.contains("malformed Pi session line 2"));
    assert!(data_home.join("token-tracker/usage.db").is_file());

    let second = run_command(&sessions, &data_home, &home);
    assert!(second.status.success());
    assert!(second.stderr.is_empty());
    assert_eq!(String::from_utf8(second.stdout).unwrap(), report);
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
