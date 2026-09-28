use std::{fmt::Display, fmt::Write, fs, io, path::Path};

use token_tracker::{
    AGENT_LABELS,
    adapters::{claude, codex, files::FileSessionSource, hermes::HermesSessionSource, pi},
    application::{ParseNotice, SessionImport, SessionSource, SnapshotCompletion, UsageStore},
    domain::Timestamp,
    storage::{SqliteUsageStore, default_database_path, validate_machine_state},
};

use super::{config::Config, reporting::escape_control_characters};

#[derive(Default)]
pub(super) struct DoctorReport {
    pub output: String,
    pub has_issues: bool,
}

pub(super) fn inspect() -> DoctorReport {
    let mut report = DoctorReport::default();
    report
        .output
        .push_str("Token Tracker doctor\n\nConfiguration:\n");
    report.config();
    report.output.push_str("\nStorage:\n");
    report.storage();
    report.output.push_str("\nSources (full scan):\n");

    report.line("Agent", "Hermes");
    match HermesSessionSource::for_default_roots() {
        Ok(source) => {
            for path in source.search_paths() {
                report.source_path(&path);
            }
            report.source(source);
        }
        Err(error) => report.issue(None, error),
    }
    report.output.push('\n');
    report.line("Agent", "Pi");
    match pi::default_session_root() {
        Ok(root) => {
            report.source_path(&root);
            report.source(FileSessionSource::new(
                pi::PiSessionDiscovery::new(root),
                pi::PiSessionParser::new(),
            ));
        }
        Err(error) => report.issue(None, error),
    }
    report.output.push('\n');
    report.line("Agent", "Codex");
    match codex::default_session_roots() {
        Ok(roots) => {
            for root in &roots {
                report.source_path(root);
            }
            report.source(FileSessionSource::new(
                codex::CodexSessionDiscovery::new(roots),
                codex::CodexSessionParser::new(),
            ));
        }
        Err(error) => report.issue(None, error),
    }
    report.output.push('\n');
    report.line("Agent", "Claude Code");
    match claude::default_session_root() {
        Ok(root) => {
            report.source_path(&root);
            report.source(FileSessionSource::new(
                claude::ClaudeSessionDiscovery::new(root),
                claude::ClaudeSessionParser::new(),
            ));
        }
        Err(error) => report.issue(None, error),
    }
    report.output.push_str(if report.has_issues {
        "\nIssues found.\n"
    } else {
        "\nNo issues found.\n"
    });
    report
}

impl DoctorReport {
    fn line(&mut self, label: &str, value: impl Display) {
        writeln!(
            self.output,
            "  {label}: {}",
            escape_control_characters(&value.to_string())
        )
        .unwrap();
    }

    fn issue(&mut self, path: Option<&Path>, error: impl Display) {
        self.has_issues = true;
        let message = match path {
            Some(path) => format!("{}: {error}", path.display()),
            None => error.to_string(),
        };
        self.line("ISSUE", message);
    }

    fn config(&mut self) {
        let config = match Config::path() {
            Some(path) => {
                self.line("Config file", path.display());
                match fs::read_to_string(&path) {
                    Ok(content) => match Config::parse(&content, path.clone()) {
                        Ok(config) => Some(config),
                        Err(error) => {
                            self.issue(None, error);
                            None
                        }
                    },
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        self.line("Config status", "not created; using defaults");
                        Some(Config::default())
                    }
                    Err(error) => {
                        self.issue(Some(&path), error);
                        None
                    }
                }
            }
            None => {
                self.line(
                    "Config file",
                    "unavailable (no absolute XDG_CONFIG_HOME or HOME); using defaults",
                );
                Some(Config::default())
            }
        };
        let Some(config) = config else { return };
        self.line(
            "machine_name",
            config.machine_name.as_deref().unwrap_or("(unset)"),
        );
        match config.server_url {
            None => self.line("server_url", "(unset)"),
            Some(value) => match super::server::parse_server_url(&value) {
                Ok(url) => self.line("server_url", url),
                Err(error) => {
                    self.line("server_url", "(invalid; value omitted)");
                    self.issue(None, error);
                }
            },
        }

        match config.auth_file {
            None => self.line("auth_file", "(unset)"),
            Some(path) => match std::path::absolute(&path) {
                Ok(path) => {
                    self.line("auth_file", path.display());
                    match token_tracker::auth::read_token(&path) {
                        Ok(_) => {
                            self.line("Auth status", "readable; token format and permissions OK")
                        }
                        Err(error) => self.issue(None, error),
                    }
                }
                Err(error) => self.issue(Some(&path), error),
            },
        }
    }

    fn storage(&mut self) {
        let path = match default_database_path() {
            Ok(path) => path,
            Err(error) => {
                self.issue(None, error);
                return;
            }
        };
        self.line("Usage database", path.display());
        let exists = self.database(&path);
        let machine_path = path.with_file_name("machine-state.db");
        self.line("Machine state", machine_path.display());
        if self.database(&machine_path) {
            if let Err(error) = validate_machine_state(&machine_path) {
                self.issue(Some(&machine_path), error);
            }
        }
        self.line("Write access", "not tested; database checks are read-only");
        if !exists {
            return;
        }
        let store = match SqliteUsageStore::open_read_only(&path) {
            Ok(store) => store,
            Err(error) => {
                self.issue(Some(&path), error);
                return;
            }
        };
        self.line(
            "Retained import issues",
            "includes sources no longer on disk",
        );
        for (agent, label) in AGENT_LABELS {
            match store.source_states(&(*agent).into()) {
                Ok(states) => {
                    for state in states {
                        let source_path = state.path.as_deref();
                        match state.last_import {
                            None => {
                                self.issue(source_path, format!("{label}: no successful import"))
                            }
                            Some(import) => {
                                if import.completion == SnapshotCompletion::Partial {
                                    self.issue(
                                        source_path,
                                        format!("{label}: last import was partial"),
                                    );
                                }
                                if import.revision != state.last_observed_revision {
                                    self.issue(
                                        source_path,
                                        format!("{label}: last observed revision was not imported"),
                                    );
                                }
                                for notice in &import.notices {
                                    self.notice(source_path, notice);
                                }
                            }
                        }
                    }
                }
                Err(error) => self.issue(Some(&path), error),
            }
        }
    }

    fn database(&mut self, path: &Path) -> bool {
        match fs::metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.line("Status", "not created");
                // Check the nearest existing parent without creating directories.
                let mut parent = path.parent();
                while let Some(directory) = parent {
                    match fs::read_dir(directory) {
                        Ok(_) => break,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {
                            parent = directory.parent()
                        }
                        Err(error) => {
                            self.issue(Some(directory), error);
                            break;
                        }
                    }
                }
                return false;
            }
            Err(error) => {
                self.issue(Some(path), error);
                return false;
            }
            Ok(metadata) => {
                if !metadata.is_file() {
                    self.issue(Some(path), "expected a regular database file");
                    return false;
                }
                if metadata.permissions().readonly() {
                    self.issue(
                        Some(path),
                        "file has no write permission; imports or exports may fail",
                    );
                }
            }
        }
        let check = || -> Result<String, rusqlite::Error> {
            let connection = rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            connection.query_row("PRAGMA quick_check", [], |row| row.get(0))
        };
        match check() {
            Ok(result) if result == "ok" => {
                self.line("Status", "readable; SQLite quick_check OK");
                true
            }
            Ok(result) => {
                self.issue(Some(path), result);
                false
            }
            Err(error) => {
                self.issue(Some(path), error);
                false
            }
        }
    }

    fn source_path(&mut self, path: &Path) {
        match std::path::absolute(path) {
            Ok(path) => {
                self.line("Search path", path.display());
                match fs::symlink_metadata(&path) {
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        self.line("Status", "not found (optional source)")
                    }
                    Err(error) => self.issue(Some(&path), error),
                }
            }
            Err(error) => self.issue(Some(path), error),
        }
    }

    fn source(&mut self, source: impl SessionSource) {
        let discovery = match source.discover(&[]) {
            Ok(discovery) => discovery,
            Err(error) => {
                self.issue(None, error);
                return;
            }
        };
        self.line("Detected sessions", discovery.sources.len());
        for warning in discovery.warnings {
            self.issue(warning.path.as_deref(), warning.message);
        }
        let mut valid = 0;
        let mut partial = 0;
        let mut failed = 0;
        for discovered in discovery.sources {
            let path = discovered.path.as_deref();
            match source.load(&discovered) {
                Ok(snapshot) => {
                    let incomplete = snapshot.session.completion == SnapshotCompletion::Partial;
                    for notice in &snapshot.session.notices {
                        self.notice(path, notice);
                    }
                    let import = SessionImport {
                        normalization_version: source.normalization_version(),
                        source: token_tracker::application::DiscoveredSource {
                            revision: snapshot.revision,
                            ..discovered.clone()
                        },
                        scanned_at: Timestamp::from_unix_milliseconds(0),
                        session: snapshot.session,
                    };
                    match import.validate(&source.agent_id()) {
                        Ok(_) => {
                            valid += 1;
                            if incomplete {
                                partial += 1;
                                self.issue(path, "partial snapshot; import would need retrying");
                            }
                        }
                        Err(error) => {
                            failed += 1;
                            self.issue(path, error);
                        }
                    }
                }
                Err(error) => {
                    failed += 1;
                    self.issue(path, error);
                }
            }
        }
        self.line(
            "Import check",
            format!("{valid} valid, {partial} partial, {failed} failed"),
        );
    }

    fn notice(&mut self, path: Option<&Path>, notice: &ParseNotice) {
        let mut message = format!(
            "{}: {} (count: {})",
            notice.code, notice.message, notice.count
        );
        if let Some(line) = notice.line {
            write!(message, ", first affected line: {line}").unwrap();
        }
        self.issue(path, message);
    }
}
