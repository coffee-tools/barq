use std::str::FromStr;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use lampo_common::conf::Network;

// FIXME: this should be a builder pattern.
use crate::algorithms::get_algorithm;
use crate::graph::NetworkGraph;

#[derive(Debug, PartialEq, Eq)]
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

    fn set_network(&mut self, network: &str) -> anyhow::Result<()>;
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
    /// Strategy to use for routing
    pub strategy: StrategyKind,
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

/// Represents a payment router within Barq.
///
/// The `Router` struct manages payment routing by choosing the best strategy
/// based on input and context. It holds a collection of routing strategies
/// (`strategies`) that can be easily extended and customized.
///
/// # Purpose
/// The main goal of the `Router` struct is to simplify payment routing in Core
/// Lightning. It acts as a central hub for coordinating different routing
/// strategies, making it easy to integrate new algorithms into the routing
/// process.
pub struct Router {
    // FIXME: Should we have a database here to store routing information?
    pub network: String,
}

impl Router {
    /// Create a new `Router` instance with the provided strategies
    pub fn new(network: &str) -> Self {
        // FIXME: Should `strategies` be optional?
        //        The default could be all strategies available
        Router {
            network: network.to_owned(),
        }
    }

    /// Execute the routing process using the best strategy based on input
    pub fn execute(&self, input: &RouteInput) -> Result<RouteOutput> {
        let mut strategy = &input.strategy;

        // rapid gossip sync can only be used with the probabilistic strategy
        if input.use_rapid_gossip_sync && input.strategy != StrategyKind::Probabilistic {
            log::warn!(
                "Rapid gossip sync can only be used with the probabilistic strategy. Ignoring specified strategy {:?}. Using probabilistic strategy instead.",
                input.strategy
            );
            strategy = &StrategyKind::Probabilistic;
        }

        let mut best_strategy = get_algorithm(strategy)
            .ok_or_else(|| anyhow::anyhow!("No strategy found for the given input"))?;
        best_strategy.set_network(&self.network)?;
        best_strategy.route(input)
    }

    /// Select the best strategy based on input
    // FIXME: we di not overdesign, would be cool to have this functionality tho
    fn select_best_strategy(&self, _input: &RouteInput) -> anyhow::Result<Option<StrategyKind>> {
        // TODO: Implement logic to select the best strategy based on input
        //       and whether the strategy can be applied to the input

        // For now, we will just use the direct strategy
        Ok(Some(StrategyKind::Direct))
    }
}
