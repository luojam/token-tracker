//! Pi discovery and parsing; wire-format details stay within this adapter.

mod discovery;
mod parsing;

pub use discovery::{PiDiscoveryError, PiSessionDiscovery, default_session_root};
pub use parsing::{PiParseError, PiSessionParser};

const PI_AGENT_ID: &str = "pi";
