//! Background work (docs/design/06): the durable queue and its workers, and the
//! ticks that feed it; handlers arrive in P6-05 and P6-06.

pub mod poll;
pub mod scheduler;
pub mod ticks;

/// Job kinds (v1 `JOB`, design/06 §Job catalogue).
pub mod kinds {
    pub const POLL_LIVE: &str = "poll:live";
    pub const POLL_RANK: &str = "poll:rank";
    pub const POLL_MATCHES: &str = "poll:matches";
    pub const ARCHIVE_MATCH: &str = "archive:match";
    pub const BACKFILL_PLAYER: &str = "backfill:player";
    pub const DDRAGON_SYNC: &str = "ddragon:sync";
    pub const MAINTENANCE: &str = "maintenance";
}

/// design/06 §Priority bands: lower runs first.
pub mod priority {
    /// `archive:match` for a game that just finished, `?refresh=true`.
    pub const INTERACTIVE: i64 = 0;
    /// `archive:match` by depth: `ARCHIVE_DEPTH + depth / 10` (v1 #31).
    pub const ARCHIVE_DEPTH: i64 = 100;
    pub const POLL: i64 = 10_000;
    /// `backfill:player`, `ladder:*`.
    pub const BACKFILL: i64 = 20_000;
    /// `aggregate:analytics`, `facts:reextract`, `maintenance`, `ddragon:sync`.
    pub const MAINTENANCE: i64 = 30_000;
}

pub use scheduler::{
    Enqueued, Handler, Job, JobError, NewJob, Queue, Registry, Scheduler, Workers, enqueue_on,
};
