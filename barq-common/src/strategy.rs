use std::str::FromStr;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use lampo_common::conf::Network;

use crate::graph::NetworkGraph;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum StrategyKind {
    Direct,
    Probabilistic,
}

impl FromStr for StrategyKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_lowercase().as_str() {
            "direct" => Ok(Self::Direct),
            "probabilistic" => Ok(Self::Probabilistic),
            _ => anyhow::bail!("Strategy `{s}` not found"),
        }
    }
}

impl Default for StrategyKind {
    fn default() -> Self {
        Self::Direct
    }
}

/// The `Strategy` trait defines an interface for routing strategies used within
/// Barq.
///
/// This trait encapsulates the core logic for finding a payment route based on
/// a specific routing algorithm. Implementations of this trait are responsible
/// for processing `RouteInput` and producing `RouteOutput`.
pub trait Strategy {
    /// Whether the strategy can be applied to the given input
    fn can_apply(&self, input: &RouteInput) -> Result<bool>;

    /// Route the payment using the strategy
    /// return error if execution unsuccessful
    fn route(&self, input: &RouteInput) -> Result<RouteOutput>;
}

/// Represents a single hop in a route between two nodes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteHop {
    pub id: String,
    pub channel: String,
    pub delay: u32,
    pub amount_msat: u64,
}

impl RouteHop {
    /// Create a new `RouteHop` instance with the provided fields
    pub fn new(id: String, channel: String, delay: u32, amount_msat: u64) -> Self {
        RouteHop {
            id,
            channel,
            delay,
            amount_msat,
        }
    }
}

/// Represents input data required for routing a payment
pub struct RouteInput {
    pub src_pubkey: String,
    pub dest_pubkey: String,
    pub network: Network,
    pub amount_msat: u64,
    pub cltv: u64,
    /// The network graph used for routing
    pub graph: Box<dyn NetworkGraph>,
    /// Whether to use the rapid gossip sync map to build the network graph
    /// (Only applicable when the probabilistic strategy is selected)
    ///
    /// If not provided, we will try to use CLN gossip map to build the network
    /// graph
    pub use_rapid_gossip_sync: bool,
}

/// Represents the output of a routing strategy
#[derive(Serialize, Deserialize)]
pub struct RouteOutput {
    pub path: Vec<RouteHop>,
}
