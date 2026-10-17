//! Shared types for connection policy, errors, limits, and URL inspection.
//! Database drivers live in the adapter crates.

mod error;
mod policy;
mod redact;
mod target;

pub use error::{DbError, ErrorCode};
pub use policy::{NetworkPolicy, ServerConfig};
pub use redact::redact_secrets;
pub use target::{parse_target, DatabaseKind, ParsedTarget};

use uuid::Uuid;

pub fn new_connection_id() -> String {
    format!("conn_{}", Uuid::new_v4().simple())
}

pub fn new_transaction_id() -> String {
    format!("txn_{}", Uuid::new_v4().simple())
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
