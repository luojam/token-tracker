mod discovery;
mod parsing;

pub use discovery::{CodexDiscoveryError, CodexSessionDiscovery, default_session_roots};
pub use parsing::{CodexParseError, CodexSessionParser};

const CODEX_AGENT_ID: &str = "codex";

const NORMALIZATION_VERSION: u32 = 1;
