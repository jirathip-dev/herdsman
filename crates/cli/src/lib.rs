//! #264: `herdsman_core` plus the server's `serve`, under the one `herdsman::` path.
pub use herdsman_core::*;
pub use herdsman_server::{hooks, serve};
