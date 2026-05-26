//! WebSocket API support — AWS API Gateway v2 WebSocket semantics.

pub mod connection;
pub mod event;
pub mod store;
pub mod upgrade;

#[allow(unused_imports)]
pub use connection::{Connection, ConnectionId, OutboundMessage};
#[allow(unused_imports)]
pub use store::ConnectionStore;
