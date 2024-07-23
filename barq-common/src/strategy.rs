use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::algorithms::direct::Direct;
use crate::algorithms::get_algorithm;
use crate::graph::NetworkGraph;

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
    pub delay: u64,
    pub amount_msat: u64,
}

impl RouteHop {
    /// Create a new `RouteHop` instance with the provided fields
    pub fn new(id: String, channel: String, delay: u64, amount_msat: u64) -> Self {
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
    pub amount_msat: u64,
    pub cltv: u64,
    /// The network graph used for routing
    pub graph: Box<dyn NetworkGraph>,
    /// Strategy to use for routing
    ///
    /// Note: This field is optional. If not provided, the router will select
    /// the best strategy based on the input and context.
    pub strategy: Option<String>,
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
    /// A collection of routing strategies that can be used to route payments
    pub strategies: Vec<Box<dyn Strategy>>,
    // FIXME: Should we have a database here to store routing information?
}

impl Default for Router {
    /// Create a new `Router` instance with the default strategies
    ///
    /// Default strategies:
    /// - [`crate::algorithms::direct::Direct`]
    fn default() -> Self {
        // SAFETY: We can safely unwrap here because we know that the algorithm exists
        let direct = get_algorithm("direct").expect("Failed to get direct algorithm");
        let strategies = vec![direct];

        Router { strategies }
    }
}

impl Router {
    /// Create a new `Router` instance with the provided strategies
    pub fn new(strategies: Vec<Box<dyn Strategy>>) -> Self {
        // FIXME: Should `strategies` be optional?
        //        The default could be all strategies available
        Router { strategies }
    }

    /// Execute the routing process using the best strategy based on input
    ///
    /// If the user specifies a strategy in the input, the router will use that
    /// strategy. Otherwise, it will select the best strategy based on the input
    /// and context.
    pub fn execute(&self, input: &RouteInput) -> Result<RouteOutput> {
        // Either the user specifies a strategy or we select the best one
        let best_strategy = match input.strategy.clone() {
            Some(strategy) => {
                let strategy = get_algorithm(&strategy)
                    .ok_or_else(|| anyhow::anyhow!("Failed to get strategy: {}", strategy))?;
                strategy
            }
            None => self
                .select_best_strategy(input)?
                .ok_or_else(|| anyhow::anyhow!("No strategy found for the given input"))?,
        };

        best_strategy.route(input)
    }

    /// Select the best strategy based on input
    fn select_best_strategy(&self, _input: &RouteInput) -> Result<Option<Box<dyn Strategy>>> {
        // TODO: Implement logic to select the best strategy based on input
        //       and whether the strategy can be applied to the input

        // For now, we will just use the direct strategy
        Ok(Some(Box::new(Direct::new())))
    }
}
