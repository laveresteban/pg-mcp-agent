//! pg-mcp-agent library surface.
//!
//! The binary ([`main.rs`](../main.rs)) is a thin CLI over these modules.
//! Exposing them as a library lets integration tests (and the bundled mock MCP
//! server) drive the same code the binary uses.

pub mod agent;
pub mod analytics;
#[cfg(feature = "datafusion")]
pub mod analytics_datafusion;
pub mod audit;
pub mod catalog;
pub mod cdc;
pub mod config;
pub mod guard;
pub mod knowledge;
pub mod mcp;
pub mod ollama;
pub mod parity;
pub mod pipeline;
pub mod router;
pub mod semantics;
pub mod specgen;
pub mod verify;
