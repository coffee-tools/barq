use serde::{Deserialize, Serialize};

use crate::graph::NetworkGraph;
use crate::algorithms::dijkstra::Dijkstra;

/// The `Strategy` trait defines an interface for routing strategies used within Barq.
///
/// This trait encapsulates the core logic for finding a payment route based on a specific routing algorithm.
/// Implementations of this trait are responsible for processing `RouteInput` and producing `RouteOutput`.
pub trait Strategy {
    /// Whether the strategy can be applied to the given input
    fn can_apply(&self, input: &RouteInput) -> bool;

    /// Route the payment using the strategy
    fn route(&self, input: &RouteInput) -> RouteOutput;
}

#[derive(Debug, Clone, Serialize, Deserialize)] 
pub struct Route {
    pub id: String,
    pub channel: String,
    pub delay: u64,
    pub fee: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)] 
pub struct RouteHop {
    pub id: String,
    pub channel: String,
    pub delay: u64,
    pub amount_msat: u64,
}

impl RouteHop {
    pub fn new(id: String, channel: String, delay: u64, amount_msat: u64) -> Self {
        RouteHop { id, channel, delay, amount_msat }
    }
}


impl Route {
    pub fn new(id: String, channel: String, delay: u64, fee: u64) -> Self {
        Route { id, channel, delay, fee }
    }
}

/// Represents input data required for routing a payment
#[derive(Serialize, Deserialize)]
pub struct RouteInput {
    pub src_pubkey: String,
    pub dest_pubkey: String,
    pub amount_msat: u64,
    pub graph: NetworkGraph
    // TODO: Add more fields as needed
}

/// Represents the output of a routing strategy
#[derive(Serialize, Deserialize)]
pub struct RouteOutput {
    pub path: Vec<RouteHop>
    // TODO: Add more fields as needed
}

/// Represents a payment router within Barq.
///
/// The `Router` struct manages payment routing by choosing the best strategy based on input and context.
/// It holds a collection of routing strategies (`strategies`) that can be easily extended and customized.
///
/// # Purpose
/// The main goal of the `Router` struct is to simplify payment routing in Core Lightning.
/// It acts as a central hub for coordinating different routing strategies, making it easy to integrate
/// new algorithms into the routing process.
pub struct Router {
    /// A collection of routing strategies that can be used to route payments
    pub strategies: Vec<Box<dyn Strategy>>,
    // FIXME: Should we have a database here to store routing information?
}

impl Router {
    /// Create a new `Router` instance with the provided strategies
    pub fn new(strategies: Vec<Box<dyn Strategy>>) -> Self {
        // FIXME: Should `strategies` be optional?
        //        The default could be all strategies available
        Router { strategies }
    }

    /// Execute the routing process using the best strategy based on input
    pub fn execute(&self, input: &RouteInput) -> RouteOutput {
        // TODO: For now, we only allow using one strategy for the whole payment
        //       Later, we can implement a more complex logic to select the best strategy
        //       where we can use multiple strategies for different parts of the payment
        //       and get the result
        let strategy = self.select_best_strategy(input);
        strategy.route(input)
    }

    /// Select the best strategy based on input
    fn select_best_strategy(&self, input: &RouteInput) -> &Box<dyn Strategy> {
        // TODO: Implement logic to select the best strategy based on input
        //       and whether the strategy can be applied to the input

        // For now, we will just use the first strategy as a placeholder
        self.strategies.first().expect("No strategies available")
    }
}
