//! scrcpy-agent: drive an Android device over scrcpy's server protocol and expose it to
//! agents through MCP, with an optional native GTK4 front-end.

pub mod android;
pub mod app;
pub mod config;
pub mod mcp;
pub mod tools;
pub mod video;

#[cfg(feature = "gui")]
pub mod ui;
