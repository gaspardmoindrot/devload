//! devload core: collection, grouping, storage, and the C ABI the Swift app links against.

pub mod collect;
pub mod docker;
pub mod engine;
pub mod ffi;
pub mod model;
pub mod prompt;
pub mod quit;
pub mod scrub;
pub mod series;
pub mod store;
pub mod sys;

/// Tests that read or change the process time zone (TZ) take this lock.
#[cfg(test)]
pub(crate) static TZ_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
