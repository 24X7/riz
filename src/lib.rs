// FIXME(wave-7): roadmap §7 covers the dead-code cleanup pass.
// Many of these dead items are pre-use scaffolding for Waves 1-6
// (WebSocket Connection/OutboundMessage, etc.). Silencing crate-wide here
// keeps CI green while Wave 7 itemizes the keep/delete decision per symbol.
#![allow(dead_code)]

pub mod cache;
pub mod config;
pub mod deploy;
pub mod gateway;
pub mod hotreload;
pub mod metrics;
pub mod process;
pub mod router;
pub mod runtime;
pub mod server;
pub mod state;
pub mod system;
pub mod tui;
pub mod ws;

#[cfg(test)]
pub mod test_helpers;
