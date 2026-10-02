//! Background work (docs/design/06): the durable queue and its workers, and the
//! ticks that feed it, and the handlers by kind.

pub mod analytics;
pub mod archive;
pub mod ddragon;
pub mod ladder;
pub mod maintenance;
pub mod names;
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
    pub const LADDER_CRAWL: &str = "ladder:crawl";
    pub const LADDER_APEX: &str = "ladder:apex";
    pub const LADDER_WALK: &str = "ladder:walk";
    pub const LADDER_COLLECT: &str = "ladder:collect";
    pub const LADDER_ARCHIVE: &str = "ladder:archive";
    pub const NAMES_BACKFILL: &str = "names:backfill";
    pub const AGGREGATE_ANALYTICS: &str = "aggregate:analytics";
    pub const FACTS_REEXTRACT: &str = "facts:reextract";
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

/// Every handler that exists, by kind.
pub fn handlers(
    poll: &std::sync::Arc<poll::PollContext>,
    archive: &std::sync::Arc<archive::ArchiveContext>,
    ddragon: &std::sync::Arc<ddragon::DdragonSync>,
    ladder: &std::sync::Arc<ladder::LadderContext>,
    names: &std::sync::Arc<names::NamesBackfill>,
    analytics: &std::sync::Arc<analytics::AnalyticsContext>,
    maintenance: &std::sync::Arc<maintenance::Maintenance>,
) -> Registry {
    use std::sync::Arc;
    Registry::new()
        .with(kinds::POLL_LIVE, poll::PollLive(Arc::clone(poll)))
        .with(kinds::POLL_RANK, poll::PollRank(Arc::clone(poll)))
        .with(kinds::POLL_MATCHES, poll::PollMatches(Arc::clone(poll)))
        .with(
            kinds::ARCHIVE_MATCH,
            archive::ArchiveMatchHandler(Arc::clone(archive)),
        )
        .with(
            kinds::BACKFILL_PLAYER,
            archive::BackfillPlayerHandler(Arc::clone(archive)),
        )
        .with(
            kinds::DDRAGON_SYNC,
            ddragon::DdragonSyncHandler(Arc::clone(ddragon)),
        )
        .with(
            kinds::LADDER_CRAWL,
            ladder::LadderCrawlHandler(Arc::clone(ladder)),
        )
        .with(kinds::LADDER_APEX, ladder::LadderApexHandler(Arc::clone(ladder)))
        .with(kinds::LADDER_WALK, ladder::LadderWalkHandler(Arc::clone(ladder)))
        .with(
            kinds::LADDER_COLLECT,
            ladder::LadderCollectHandler(Arc::clone(ladder)),
        )
        .with(
            kinds::LADDER_ARCHIVE,
            ladder::LadderArchiveHandler(Arc::clone(ladder)),
        )
        .with(
            kinds::NAMES_BACKFILL,
            names::NamesBackfillHandler(Arc::clone(names)),
        )
        .with(
            kinds::AGGREGATE_ANALYTICS,
            analytics::AggregateHandler(Arc::clone(analytics)),
        )
        .with(
            kinds::FACTS_REEXTRACT,
            analytics::ReextractHandler(Arc::clone(analytics)),
        )
        .with(
            kinds::MAINTENANCE,
            maintenance::MaintenanceHandler(Arc::clone(maintenance)),
        )
}

pub use scheduler::{
    Enqueued, Handler, Job, JobAction, JobError, JobRow, NewJob, Queue, Registry, Scheduler, Workers,
    enqueue_on,
};
