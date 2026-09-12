use std::num::NonZeroU32;

mod discovery;
mod parsing;

pub use discovery::{CodexDiscoveryError, CodexSessionDiscovery, default_session_roots};
pub use parsing::{CodexParseError, CodexSessionParser};

const CODEX_AGENT_ID: &str = "codex";

const NORMALIZATION_VERSION: NonZeroU32 = NonZeroU32::MIN;
