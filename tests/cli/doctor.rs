use super::{ALL_USAGE, CLAUDE_SESSION_ID, CODEX_USAGE, command, successful_report};
use crate::support::{TempTree, hermes};
use std::{fs, os::unix::fs::PermissionsExt};

#[test]
fn doctor_resolves_overrides_and_checks_all_sources_without_creating_local_data() {
    let tree = TempTree::new();
    tree.write("sessions/pi.jsonl", ALL_USAGE);
    tree.write("codex/archived_sessions/rollout-test.jsonl", CODEX_USAGE);
    fs::create_dir_all(tree.root.join(".hermes/profiles/work")).unwrap();
    hermes::database(&tree.root.join(".hermes/profiles/work/state.db"))
        .execute_batch("DELETE FROM session_model_usage; UPDATE sessions SET input_tokens = 0, output_tokens = 0, cache_read_tokens = 0, cache_write_tokens = 0, reasoning_tokens = 0, api_call_count = 0;")
        .unwrap();
    let token = tree.write("auth.token", "SECRET_abcdefghijklmnopqrstuvwxyz0123456789");
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let config = tree.write(
        "config/token-tracker/config.toml",
        "machine_name = 'work'\nserver_url = 'https://example.com'\nauth_file = 'auth.token'\n",
    );
    let before = fs::read(&config).unwrap();

    let output = command(&tree.root)
        .current_dir(&tree.root)
        .env("XDG_CONFIG_HOME", tree.root.join("config"))
        .env("XDG_DATA_HOME", tree.root.join("data"))
        .env("PI_CODING_AGENT_SESSION_DIR", "sessions")
        .env("CODEX_HOME", "codex")
        .arg("doctor")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let report = successful_report(output);
    for path in [
        "config/token-tracker/config.toml",
        "data/token-tracker/usage.db",
        "data/token-tracker/machine-state.db",
        "sessions",
        "codex/archived_sessions",
        ".claude/projects",
        ".hermes/profiles",
        "auth.token",
    ] {
        assert!(
            report.contains(tree.root.join(path).to_str().unwrap()),
            "{report}"
        );
    }
    for expected in [
        "machine_name: work",
        "Detected sessions: 2",
        "Detected sessions: 1",
        "No issues found.",
    ] {
        assert!(report.contains(expected), "{report}");
    }
    assert!(!tree.root.join("data").exists());
    assert_eq!(fs::read(config).unwrap(), before);

    fs::remove_file(tree.root.join("config/token-tracker/config.toml")).unwrap();
    let fresh = successful_report(
        command(&tree.root)
            .current_dir(&tree.root)
            .arg("doctor")
            .output()
            .unwrap(),
    );
    assert!(fresh.contains("not created; using defaults"), "{fresh}");
    assert!(!tree.root.join(".config").exists());
    assert!(!tree.root.join(".local").exists());
}

#[test]
fn doctor_collects_config_storage_access_and_import_failures_in_one_report() {
    let tree = TempTree::new();
    tree.write(".config/token-tracker/config.toml", "machine_name = [");
    tree.write(".local/share/token-tracker", "not a directory");
    tree.write(".hermes/state.db", "not a database");
    tree.write(".pi/agent/sessions/broken.jsonl", "{SECRET_MALFORMED}\n");
    tree.write(".codex/sessions", "not a directory");
    tree.write(
        format!(".claude/projects/project/{CLAUDE_SESSION_ID}.jsonl"),
        include_str!("../fixtures/claude/partial-ignored.jsonl"),
    );
    let output = command(&tree.root)
        .current_dir(&tree.root)
        .arg("doctor")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "could not load config",
        "Usage database:",
        "Machine state:",
        "could not read Hermes accounting snapshot",
        "could not read directory",
        "1 failed",
        "first affected line:",
        "Issues found.",
    ] {
        assert!(report.contains(expected), "missing {expected}: {report}");
    }
    assert!(!report.contains("SECRET_"), "{report}");
}

#[test]
fn doctor_reports_retained_notices_without_changing_storage_or_export_revision() {
    let tree = TempTree::new();
    let source = tree.write(
        format!(".claude/projects/project/{CLAUDE_SESSION_ID}.jsonl"),
        include_str!("../fixtures/claude/partial-ignored.jsonl"),
    );
    successful_report(
        command(&tree.root)
            .arg("export")
            .arg(tree.root.join("export.db"))
            .output()
            .unwrap(),
    );
    fs::remove_file(source).unwrap();
    let usage = tree.root.join(".local/share/token-tracker/usage.db");
    let machine = usage.with_file_name("machine-state.db");
    let before_usage = fs::read(&usage).unwrap();
    let before_machine = fs::read(&machine).unwrap();
    let output = command(&tree.root)
        .current_dir(&tree.root)
        .arg("doctor")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(report.contains("Retained import issues"), "{report}");
    assert!(report.contains("first affected line:"), "{report}");
    assert_eq!(fs::read(usage).unwrap(), before_usage);
    assert_eq!(fs::read(machine).unwrap(), before_machine);
}

#[test]
fn doctor_collapses_identical_issues_per_section_without_hiding_distinct_sources() {
    let tree = TempTree::new();
    let paths = [".hermes/state.db", ".hermes/profiles/work/state.db"];
    for (index, path) in paths.iter().enumerate() {
        let path = tree.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        hermes::database(&path)
            .execute_batch(&format!(
                "INSERT INTO sessions (id, started_at, input_tokens)
                 VALUES ('second', 1700000003, 10);
                 INSERT INTO session_model_usage
                     (session_id, model, billing_provider, billing_mode, input_tokens)
                 VALUES ('second', 'model-a', 'openai-codex', 'subscription', 10);
                 UPDATE sessions SET id = '{index}-' || id;
                 UPDATE session_model_usage SET session_id = '{index}-' || session_id;"
            ))
            .unwrap();
    }
    successful_report(command(&tree.root).output().unwrap());

    let output = command(&tree.root).arg("doctor").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report = String::from_utf8(output.stdout).unwrap();
    let (stored, scanned) = report.split_once("Sources (full scan):").unwrap();
    for section in [stored, scanned] {
        for path in paths {
            let prefix = format!("ISSUE: {}: ", tree.root.join(path).display());
            for code in ["hermes_subscription_estimate", "hermes_counter_mismatch"] {
                assert_eq!(
                    section.matches(&format!("{prefix}{code}:")).count(),
                    1,
                    "{section}"
                );
            }
        }
    }
}

#[test]
fn doctor_reports_setup_failures_for_every_agent() {
    let tree = TempTree::new();
    let output = command(&tree.root)
        .current_dir(&tree.root)
        .env("HOME", "relative-home")
        .arg("doctor")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report = String::from_utf8(output.stdout).unwrap();
    for agent in ["Hermes", "Pi", "Codex", "Claude Code"] {
        assert!(
            report.contains(&format!(
                "  Agent: {agent}\n  ISSUE: HOME is unavailable or is not an absolute path"
            )),
            "{report}"
        );
    }
}

#[test]
fn doctor_rejects_foreign_machine_state_without_changing_it() {
    let tree = TempTree::new();
    let machine = tree.write(".local/share/token-tracker/machine-state.db", "");
    rusqlite::Connection::open(&machine)
        .unwrap()
        .execute_batch("CREATE TABLE unrelated(value)")
        .unwrap();
    let before = fs::read(&machine).unwrap();

    let output = command(&tree.root).arg("doctor").output().unwrap();
    let report = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert!(report.contains("invalid machine state schema"), "{report}");
    assert_eq!(fs::read(machine).unwrap(), before);
}

#[test]
fn doctor_checks_server_settings_without_exposing_credentials() {
    let tree = TempTree::new();
    tree.write(
        ".config/token-tracker/config.toml",
        "server_url = 'https://user:SECRET_PASSWORD@example.com?token=SECRET_QUERY'\nauth_file = 'auth.token'\n",
    );
    let token = tree.write("auth.token", "SECRET_abcdefghijklmnopqrstuvwxyz0123456789");
    fs::set_permissions(token, fs::Permissions::from_mode(0o644)).unwrap();
    let output = command(&tree.root)
        .current_dir(&tree.root)
        .arg("doctor")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(
        report.contains("server_url: (invalid; value omitted)"),
        "{report}"
    );
    assert!(report.contains("chmod 600"), "{report}");
    assert!(!report.contains("SECRET_"), "{report}");
}
