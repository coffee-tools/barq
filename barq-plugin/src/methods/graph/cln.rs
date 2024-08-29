use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use clightningrpc_plugin::errors::PluginError;

use barq_common::graph::{Channel, NetworkGraph, Node};

use crate::plugin::State;

/// CLN Network Graph
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CLNNetworkGraph {
    nodes: HashMap<String, Node>,
    channels: HashMap<String, Channel>,
}

impl Default for CLNNetworkGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl CLNNetworkGraph {
    /// Creates a new, empty network graph.
    pub fn new() -> Self {
        CLNNetworkGraph {
            nodes: HashMap::new(),
            channels: HashMap::new(),
        }
    }

    /// Adds a node to the network graph.
    pub fn add_node(&mut self, node: Node) {
        self.nodes.insert(node.id.clone(), node);
    }

    /// Adds a channel to the network graph.
    pub fn add_channel(&mut self, channel: Channel) {
        self.channels
            .insert(channel.short_channel_id.clone(), channel.clone());
        self.nodes
            .get_mut(&channel.node1)
            .unwrap()
            .add_channel(&channel);
    }
}

impl NetworkGraph for CLNNetworkGraph {
    fn get_node(&self, id: &str) -> Option<&Node> {
        self.nodes.get(id)
    }

    fn get_channel(&self, id: &str) -> Option<&Channel> {
        self.channels.get(id)
    }

    fn get_nodes(&self) -> Vec<&Node> {
        self.nodes.values().collect()
    }

    fn get_channels(&self) -> Vec<&Channel> {
        self.channels.values().collect()
    }

    fn has_p2p_info(&self) -> bool {
        false
    }
}

/// Structure representing a channel as returned by the `listchannels` method.
///
/// See https://docs.corelightning.org/reference/lightning-listchannels#return-value
#[derive(Deserialize, Debug)]
struct CLNListChannelsResponse {
    channels: Vec<ChannelInfo>,
}

/// Structure representing a channel as returned by CLN `listchannels` method.
#[derive(Deserialize, Debug)]
struct ChannelInfo {
    source: String,
    destination: String,
    short_channel_id: String,
    amount_msat: u64,
    delay: u64,
    base_fee_millisatoshi: u64,
    fee_per_millionth: u64,
}

/// Function to build the network graph using the plugin state.
pub fn build_cln_network_graph(state: &State) -> Result<CLNNetworkGraph, PluginError> {
    // Call the `listchannels` method to get the network information
    let response: CLNListChannelsResponse = state
        .call("listchannels", serde_json::json!({}))
        .map_err(|err| PluginError::new(err.code, &err.message, err.data))?;

    let mut graph = CLNNetworkGraph::new();

    // Iterate over the channels to construct the nodes and edges
    for channel in response.channels {
        // Convert amount_msat to u64
        let amount_msat = channel.amount_msat;

        // Add channel to the graph
        let edge = Channel::new(
            &channel.short_channel_id,
            &channel.source,
            &channel.destination,
            amount_msat,
            channel.delay,
            channel.base_fee_millisatoshi,
            channel.fee_per_millionth,
        );
        graph.add_channel(edge);
    }

    Ok(graph)
}
