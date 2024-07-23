use serde::{Deserialize, Serialize};
use serde_json as json;
use serde_json::Value;

use clightningrpc_gossip_map::GossipMap;
use clightningrpc_plugin::error;
use clightningrpc_plugin::errors::PluginError;
use clightningrpc_plugin::plugin::Plugin;

use barq_common::strategy::{RouteHop, RouteInput, Router};

use crate::methods::pay::NodeInfo;
use crate::methods::utils::graph::build_network_graph;
use crate::plugin::State;

/// Request payload for Barq route info RPC method
#[derive(Deserialize, Serialize)]
pub struct BarqRouteInfoRequest {
    pub dest_pubkey: String,
    pub amount_msat: u64,
    pub cltv: u64,
    /// The strategy to use for routing the payment
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
}

/// Response payload for Barq route info RPC method
#[derive(Deserialize, Serialize)]
pub struct BarqRouteInfoResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_info: Option<Vec<RouteHop>>,
}

/// Barq RPC method to get route information
pub fn barq_route_info(plugin: &mut Plugin<State>, request: Value) -> Result<Value, PluginError> {
    log::info!("barqrouteinfo called with request: {}", request);
    let request: BarqRouteInfoRequest = json::from_value(request).map_err(|err| error!("{err}"))?;

    let state = &plugin.state;
    let router = Router::default();

    let node_info: NodeInfo = state
        .call("getinfo", serde_json::json!({}))
        .map_err(|err| error!("Error calling CLN RPC method: {err}"))?;

    // Get the gossip map path from the plugin state

    // This is for example: /home/user/.lightning/lightning-rpc
    let lightning_rpc_path = state
        .cln_rpc_path()
        .ok_or_else(|| error!("CLN RPC path not found in the plugin state"))?;
    let lightning_rpc_path = std::path::Path::new(&lightning_rpc_path);
    // Lightning path is /home/user/.lightning
    let lightning_path = lightning_rpc_path.parent().ok_or_else(|| {
        error!(
            "Failed to get parent directory of CLN RPC path: {:?}",
            lightning_rpc_path
        )
    })?;
    // Gossip map path is /home/user/.lightning/<network>/gossip_store
    let gossip_map_path = lightning_path.join(node_info.network).join("gossip_store");
    let gossip_map_path = gossip_map_path.to_str().ok_or_else(|| {
        error!(
            "Failed to convert gossip map path to string: {:?}",
            gossip_map_path
        )
    })?;
    let gossip_map = GossipMap::from_file(gossip_map_path)
        .map_err(|err| error!("Error reading gossip map from file: {err}"))?;

    // Build the network graph from the plugin state
    let network_graph = build_network_graph(state, &gossip_map)?;

    let input = RouteInput {
        src_pubkey: node_info.id.clone(),
        dest_pubkey: request.dest_pubkey.clone(),
        amount_msat: request.amount_msat,
        cltv: request.cltv,
        graph: network_graph,
        strategy: request.strategy,
    };

    let output = router.execute(&input);
    let response = match output {
        Ok(output) => BarqRouteInfoResponse {
            status: "success".to_string(),
            message: None,
            route_info: Some(output.path),
        },
        Err(err) => BarqRouteInfoResponse {
            status: "failure".to_string(),
            message: Some(format!("barqrouteinfo execution failed: {}", err)),
            route_info: None,
        },
    };

    Ok(json::to_value(response)?)
}
