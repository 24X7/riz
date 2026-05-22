//! Riz system functions mounted under /_riz/*.
//! Each handler implements LambdaHandler and reads from RizState.

pub mod health;
pub mod metrics;
pub mod registry;
pub mod mcp;
