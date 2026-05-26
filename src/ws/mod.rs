//! WebSocket API support — AWS API Gateway v2 WebSocket semantics.

pub mod connection;
pub mod store;

#[allow(unused_imports)]
pub use connection::{Connection, ConnectionId, OutboundMessage};
#[allow(unused_imports)]
pub use store::ConnectionStore;
