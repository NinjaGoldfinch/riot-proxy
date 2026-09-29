//! Background work (docs/design/06): the durable queue and its workers; ticks
//! and handlers arrive in P6-04 to P6-06.

pub mod scheduler;

pub use scheduler::{Enqueued, Handler, Job, JobError, NewJob, Registry, Scheduler, Workers, enqueue_on};
