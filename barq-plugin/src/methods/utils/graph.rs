use serde::Deserialize;
use serde::Serialize;

use clightningrpc_plugin::{error, errors::PluginError, plugin::Plugin};

use crate::plugin::State;
use barq_common::graph::{Edge, NetworkGraph, Node};

/// Structure representing a channel as returned by the `listchannels` method.
///
/// See https://docs.corelightning.org/reference/lightning-listchannels#return-value
#[derive(Deserialize, Debug)]
struct ListChannelsResponse {
    channels: Vec<ChannelInfo>,
}

#[derive(Deserialize, Debug)]
struct ChannelInfo {
    source: String,
    destination: String,
    short_channel_id: String,
    amount_msat: u64,
    delay: u64,
    base_fee_millisatoshi: u64,
    fee_per_millionth: u64
}

/// Function to build the network graph using the plugin state.
pub fn build_network_graph(state: &State) -> Result<NetworkGraph, PluginError> {

    // Call the `listchannels` method to get the network information
    let response : ListChannelsResponse = state
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
        let amount_msat = channel
            .amount_msat
            .to_string()
            .trim_end_matches("msat")
            .parse::<u64>()
            .unwrap_or(0);

        // Add edge to the graph
        let edge = Edge::new(
            &channel.short_channel_id,
            &channel.source,
            &channel.destination,
            amount_msat,
            channel.delay,
            channel.base_fee_millisatoshi,
            channel.fee_per_millionth
        );
        graph.add_edge(edge);
    }

    Ok(graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clightningrpc_plugin::plugin::Plugin;

    #[test]
    fn test_build_network_graph() {
        // TODO: Implement proper tests when integrating with a real or mocked plugin state.

        let mut plugin = Plugin::new(State::new(), false);

        // Call the function (this won't actually work without a proper plugin state setup)
        match build_network_graph(&mut plugin) {
            Ok(graph) => {
                // Check the graph contents
                assert!(graph.get_all_nodes().is_empty());
                assert!(graph.get_all_edges().is_empty());
            }
            Err(err) => {
                // Handle error (expected in this dummy test)
                println!("Error: {:?}", err);
            }
        }
    }
}
