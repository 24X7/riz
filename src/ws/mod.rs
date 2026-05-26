//! WebSocket API support — AWS API Gateway v2 WebSocket semantics.

pub mod connection;

pub use connection::{Connection, ConnectionId, OutboundMessage};
