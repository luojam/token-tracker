use std::num::NonZeroU32;

mod discovery;
mod parsing;

pub use discovery::{ClaudeDiscoveryError, ClaudeSessionDiscovery, default_session_root};
pub use parsing::{ClaudeParseError, ClaudeSessionParser};

pub(crate) const CLAUDE_AGENT_ID: &str = "claude";

const NORMALIZATION_VERSION: NonZeroU32 = NonZeroU32::new(2).unwrap();
