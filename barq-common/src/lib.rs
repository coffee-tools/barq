//! Barq core logic: routing strategies built on top of LDK, the shared
//! network graph (fed by Core Lightning gossip and/or LDK rapid gossip
//! sync) and the onion payload builder used to hand a route to Core
//! Lightning.
pub mod algorithms;
pub mod gossip_store;
pub mod network;
pub mod onion;
pub mod scid;
pub mod strategy;

pub use lampo_common::conf::Network;
pub use lampo_common::{bitcoin, ldk, secp256k1};
