use serde::{Deserialize, Serialize};
use serde_json as json;
use serde_json::Value;

use clightningrpc_common::{client::Client, types::Response};
use clightningrpc_gossip_map::prelude::bolt7::ChannelAnnouncement;
use clightningrpc_plugin::{error, errors::PluginError, plugin::Plugin};

use barq_common::{graph::NetworkGraph, strategy::RouteInput};

use crate::{methods::utils::graph::build_network_graph, plugin::State};

/// Request payload for Barq pay RPC method
#[derive(Deserialize, Serialize)]
pub struct BarqPayRequest {
    pub payment_hash: String,
    pub amount: u64,
    pub destination: String,
    // TODO: Add more fields as needed
}

/// Response payload for Barq pay RPC method
#[derive(Deserialize, Serialize)]
pub struct BarqPayResponse {
    pub status: String,
    pub message: String,
    pub route: String,
    // TODO: Add more fields as needed
}

/// Barq RPC method to execute a payment
pub fn barq_pay(plugin: &mut Plugin<State>, request: Value) -> Result<Value, PluginError> {
    log::info!("barqpay called with request: {}", request);
    let request: BarqPayRequest = json::from_value(request).map_err(|err| error!("{err}"))?;

    // TODO: Implement the logic to execute a payment using Barq strategies
    let state = &plugin.state;
    let router = state.router();

    let socket = state
        .cln_rpc_path()
        .ok_or_else(|| {
            error!("CLN RPC path not found in the plugin state");
        })
        .unwrap(); // TODO: Handle error

    let client = Client::new(&socket);

    // TODO: use CLN RPC to query information needed about gossip map

    // TODO: use CLN RPC to query any information needed
    let cln_response = state
        .call("getinfo", ())
        .map_err(|err| error!("Error calling CLN RPC method: {err}"))?;

    // Build the network graph from the plugin state
    let network_graph = build_network_graph(plugin)?;

    // TODO: Constrcut `RouteInput` from the request and CLN information gathered
    let input = RouteInput {
        source: request.payment_hash.clone(),
        destination: request.destination.clone(),
        amount: request.amount,
        graph: network_graph,
    };

    // Execute the routing process
    let output = router.execute(&input);

    // Construct the response from the output
    let response = BarqPayResponse {
        status: "success".to_string(),
        message: "Payment executed successfully".to_string(),
        route: output.path.join(" -> "),
    };
    Ok(json::to_value(response)?)
}
