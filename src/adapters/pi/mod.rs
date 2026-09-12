use std::num::NonZeroU32;

mod discovery;
mod parsing;

pub use discovery::{PiDiscoveryError, PiSessionDiscovery, default_session_root};
pub use parsing::{PiParseError, PiSessionParser};

use super::registry::PI_AGENT_ID;

const NORMALIZATION_VERSION: NonZeroU32 = NonZeroU32::MIN;
