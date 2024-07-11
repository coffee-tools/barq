use serde::Deserialize;

use clightningrpc_gossip_map::GossipMap;
use clightningrpc_plugin::error;
use clightningrpc_plugin::errors::PluginError;

use barq_common::graph::{Edge, NetworkGraph, Node};

use crate::plugin::State;

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
pub fn build_network_graph(
    state: &State,
    gossip_map: &GossipMap,
) -> Result<NetworkGraph, PluginError> {
    // Call the `listchannels` method to get the network information
    let response: CLNListChannelsResponse = state
        .call("listchannels", serde_json::json!({}))
        .map_err(|err| error!("Error calling `listchannels`: {err}"))?;

    let mut graph = NetworkGraph::new();

    // Iterate over the channels to construct the nodes and edges
    for channel in response.channels {
        // Add nodes to the graph
        if graph.get_node(&channel.source).is_none() {
            graph.add_node(Node::new(&channel.source));
        }
        if graph.get_node(&channel.destination).is_none() {
            graph.add_node(Node::new(&channel.destination));
        }

        // Convert amount_msat to u64
        let amount_msat = channel.amount_msat;

        // TODO: get more information from the gossip map
        let _channel = gossip_map.get_channel(&channel.short_channel_id);

        // Add edge to the graph
        let edge = Edge::new(
            &channel.short_channel_id,
            &channel.source,
            &channel.destination,
            amount_msat,
            channel.delay,
            channel.base_fee_millisatoshi,
            channel.fee_per_millionth,
        );
        graph.add_edge(edge);
    }

    Ok(graph)
}
