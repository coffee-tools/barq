use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json as json;
use serde_json::Value;

use clightningrpc_plugin::error;
use clightningrpc_plugin::errors::PluginError;
use clightningrpc_plugin::plugin::Plugin;

use barq_common::graph::NetworkGraph;
use barq_common::strategy::{RouteHop, RouteInput, Router, StrategyKind};

use crate::methods::graph::cln::build_cln_network_graph;
use crate::methods::graph::p2p::build_p2p_network_graph;
use crate::methods::pay::NodeInfo;
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

impl BarqRouteInfoRequest {
    pub fn strategy(&self) -> anyhow::Result<StrategyKind> {
        if let Some(ref s) = self.strategy {
            return StrategyKind::from_str(s);
        }
        Ok(StrategyKind::default())
    }
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

    // If the probabilistic strategy is selected, build the network graph from the
    // gossip map. Else, build the network graph from the plugin state
    let network_graph: Box<dyn NetworkGraph> =
        match request.strategy().map_err(|e| error!("{e}"))? {
            StrategyKind::Direct => Box::new(build_cln_network_graph(state)?),
            StrategyKind::Probabilistic => Box::new(build_p2p_network_graph(state)?),
        };
    let input = RouteInput {
        src_pubkey: node_info.id.clone(),
        dest_pubkey: request.dest_pubkey.clone(),
        amount_msat: request.amount_msat,
        cltv: request.cltv,
        graph: network_graph,
        strategy: request.strategy().map_err(|e| error!("{e}"))?,
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
