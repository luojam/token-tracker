use std::{error::Error, fmt};

use crate::domain::export::ExportSnapshot;

/// Atomically replaces a machine's snapshot.
pub trait ExportSink {
    type Error: Error + Send + Sync + 'static;

    fn publish(
        &mut self,
        snapshot: &ExportSnapshot,
    ) -> Result<PublishOutcome, PublishError<Self::Error>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishOutcome {
    Published,
    AlreadyPublished,
}

#[derive(Debug)]
pub enum PublishError<E> {
    StaleRevision {
        incoming_revision: u64,
        published_revision: u64,
    },
    RevisionConflict {
        revision: u64,
    },
    InvalidSnapshot {
        reason: String,
    },
    /// Commit may have succeeded; retry the same snapshot.
    Destination(E),
}

impl<E: fmt::Display> fmt::Display for PublishError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleRevision {
                incoming_revision,
                published_revision,
            } => write!(
                formatter,
                "export revision {incoming_revision} is older than published revision {published_revision}"
            ),
            Self::RevisionConflict { revision } => {
                write!(
                    formatter,
                    "export revision {revision} has a different payload"
                )
            }
            Self::InvalidSnapshot { reason } => {
                write!(formatter, "invalid export snapshot: {reason}")
            }
            Self::Destination(source) => write!(formatter, "export destination failed: {source}"),
        }
    }
}

impl<E: Error + 'static> Error for PublishError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Destination(source) => Some(source),
            _ => None,
        }
    }
}
