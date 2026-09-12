use crate::adapters::files::FileSessionSource;
use std::error::Error;

use super::claude::{ClaudeSessionDiscovery, ClaudeSessionParser};
use super::codex::{CodexSessionDiscovery, CodexSessionParser};
use super::pi::{PiSessionDiscovery, PiSessionParser};
use crate::application::ImportAdapter;
use crate::storage::SqliteUsageStore;

pub(super) const PI_AGENT_ID: &str = "pi";
pub(super) const CODEX_AGENT_ID: &str = "codex";
pub(super) const CLAUDE_AGENT_ID: &str = "claude";

type AdapterFactory =
    fn() -> Result<Box<dyn ImportAdapter<SqliteUsageStore>>, Box<dyn Error + Send + Sync>>;

pub(crate) struct AdapterRegistration {
    pub id: &'static str,
    pub label: &'static str,
    pub factory: AdapterFactory,
}

pub(crate) const ADAPTERS: &[AdapterRegistration] = &[
    AdapterRegistration {
        id: PI_AGENT_ID,
        label: "Pi",
        factory: || {
            Ok(Box::new(FileSessionSource::new(
                PiSessionDiscovery::for_default_root()?,
                PiSessionParser::new(),
            )))
        },
    },
    AdapterRegistration {
        id: CODEX_AGENT_ID,
        label: "Codex",
        factory: || {
            Ok(Box::new(FileSessionSource::new(
                CodexSessionDiscovery::for_default_roots()?,
                CodexSessionParser::new(),
            )))
        },
    },
    AdapterRegistration {
        id: CLAUDE_AGENT_ID,
        label: "Claude Code",
        factory: || {
            Ok(Box::new(FileSessionSource::new(
                ClaudeSessionDiscovery::for_default_root()?,
                ClaudeSessionParser::new(),
            )))
        },
    },
];

pub(crate) fn display_label(id: &str) -> &str {
    ADAPTERS
        .iter()
        .find(|adapter| adapter.id == id)
        .map_or(id, |adapter| adapter.label)
}
