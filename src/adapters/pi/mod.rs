use std::num::NonZeroU32;

mod discovery;
mod parsing;

pub use discovery::{PiDiscoveryError, PiSessionDiscovery, default_session_root};
pub use parsing::{PiParseError, PiSessionParser};

const PI_AGENT_ID: &str = "pi";

const NORMALIZATION_VERSION: NonZeroU32 = NonZeroU32::MIN;
