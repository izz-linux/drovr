//! drovr core: shared state, agent lifecycle, the coordination MCP server,
//! and Jira / Confluence / Slack lookups. Used by the desktop app and the CLI.

pub mod config;
pub mod confluence;
pub mod engine;
pub mod harness;
pub mod ledger;
pub mod mcp;
pub mod prompts;
pub mod reports;
pub mod retro;
pub mod review;
pub mod rules;
pub mod sessions;
pub mod shell;
pub mod sources;
pub mod stages;
pub mod store;
pub mod watch;

pub use config::Config;
pub use engine::{Engine, NewAgent};
pub use store::Store;
