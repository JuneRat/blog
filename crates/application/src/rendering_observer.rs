//! Bounded rendering telemetry events; adapters own the clock and execution.
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub enum RenderKind {
    Content,
    Comment,
    Theme,
}

impl RenderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::Comment => "comment",
            Self::Theme => "theme",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum QueueOutcome {
    Admitted,
    Timeout,
    Cancelled,
    Closed,
}

impl QueueOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Closed => "closed",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum RenderingEvent {
    Queued,
    QueueFinished {
        elapsed: Duration,
        outcome: QueueOutcome,
    },
    Started,
    Finished {
        elapsed: Duration,
        success: bool,
    },
    ExecutionTimeout,
}

/// Implementations must be quick, non-blocking and must not panic. Events carry
/// no source text, resource identifiers or unbounded labels.
pub trait RenderingObserver: Send + Sync {
    fn observe(&self, kind: RenderKind, event: RenderingEvent);
}
