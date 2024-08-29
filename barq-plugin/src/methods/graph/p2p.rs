#![allow(unused)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use clightningrpc_gossip_map::GossipMap;
use clightningrpc_plugin::error;
use clightningrpc_plugin::errors::PluginError;

use barq_common::graph::{Channel, NetworkGraph, Node};

use crate::plugin::State;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct P2PNetworkGraph {
    nodes: HashMap<String, Node>,
    channels: HashMap<String, Channel>,
}

impl Default for P2PNetworkGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl P2PNetworkGraph {
    /// Creates a new, empty network graph.
    pub fn new() -> Self {
        P2PNetworkGraph {
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
            .unwrap_or(&mut Node::new(&channel.node1))
            .add_channel(&channel);
        self.nodes
            .get_mut(&channel.node2)
            .unwrap_or(&mut Node::new(&channel.node1))
            .add_channel(&channel);
    }
}

impl NetworkGraph for P2PNetworkGraph {
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
        true
    }
}

/// Function to build the network graph using the plugin state.
pub fn build_p2p_network_graph(state: &State) -> Result<P2PNetworkGraph, PluginError> {
    let mut graph = P2PNetworkGraph::new();
    // Get the gossip map path from the plugin state
    // FIXME: Currently, we are loading the gossip map from the file system
    //        each time the `barqpay` method is called. This is not efficient.
    //        We should load the gossip map once and cache it in the plugin state.
    //        See: https://github.com/tareknaser/barq/issues/21 for more details.

    // SAFETY: It is safe to unwrap here because the plugin init the path always.
    let lightning_rpc_path = state.cln_rpc_path.as_ref().unwrap();
    let lightning_rpc_path = std::path::Path::new(&lightning_rpc_path);
    // Lightning path is /home/user/.lightning
    let lightning_path = lightning_rpc_path.parent().ok_or_else(|| {
        error!(
            "Failed to get parent directory of CLN RPC path: {:?}",
            lightning_rpc_path
        )
    })?;
    // SAFETY: It is safe to unwrap here because the plugin init the network always.
    let gossip_map_path = lightning_path.join("gossip_store");
    let gossip_map_path = gossip_map_path.to_str().ok_or_else(|| {
        error!(
            "Failed to convert gossip map path to string: {:?}",
            gossip_map_path
        )
    })?;
    let gossip_map = GossipMap::from_file(gossip_map_path)
        .map_err(|err| error!("Error reading gossip map from file: {err}"))?;

    for channel in gossip_map.channels.values() {
        graph.add_channel(channel.clone().into())
    }
    Ok(graph)
}
