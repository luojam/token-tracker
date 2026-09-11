mod discovery;
mod parsing;

pub use discovery::{ClaudeDiscoveryError, ClaudeSessionDiscovery, default_session_root};
pub use parsing::{ClaudeParseError, ClaudeSessionParser};

const CLAUDE_AGENT_ID: &str = "claude";

const NORMALIZATION_VERSION: u32 = 1;
