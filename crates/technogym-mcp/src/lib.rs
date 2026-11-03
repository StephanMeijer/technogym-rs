#![warn(missing_docs)]
#![expect(
    clippy::unused_async_trait_impl,
    reason = "rmcp's and axum's traits are async whether or not a method awaits"
)]
//! An MCP server over [`technogym`]: six read-only tools on stdio, or hosted over streamable
//! HTTP behind a stateless OAuth 2.1 sign-in.

pub mod config;
pub mod errors;
pub mod http;
pub mod results;
pub mod source;
pub mod tools;

pub use source::{ApiSource, FileSource, FixedSource, LOGIN_HINT, RECONNECT_HINT};
pub use tools::TechnogymMcp;
