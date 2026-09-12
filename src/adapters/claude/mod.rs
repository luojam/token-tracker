use std::num::NonZeroU32;

mod discovery;
mod parsing;

pub use discovery::{ClaudeDiscoveryError, ClaudeSessionDiscovery, default_session_root};
pub use parsing::{ClaudeParseError, ClaudeSessionParser};

use super::registry::CLAUDE_AGENT_ID;

const NORMALIZATION_VERSION: NonZeroU32 = NonZeroU32::MIN;
