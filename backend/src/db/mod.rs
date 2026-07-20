mod access;
mod body_metrics;
mod catalogue;
mod conversation;
mod dashboard;
mod database;
mod entries;
mod exercise_types;
mod goals;
mod groups;
mod health;
mod metrics;
// `pub(crate)` so `crate::dump` can reach `V2_USER_VERSION` when it decides whether a database is
// legacy, without publishing the migration set itself.
pub(crate) mod migrations;
mod models;
mod philosophy;
mod programmes;
mod progress;
mod rosters;
mod users;

pub use body_metrics::canonical_body_metric;
pub use database::Database;
pub use entries::{EntryReclassifyOutcome, EntryWithSets, SessionWithSets, SetEdit, SetEditError, SetEditOutcome};
pub use models::*;
