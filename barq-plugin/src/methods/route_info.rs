//! `barqrouteinfo`: compute a route without paying.
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json as json;
use serde_json::Value;

use clightningrpc_plugin::error;
use clightningrpc_plugin::errors::PluginError;
use clightningrpc_plugin::plugin::Plugin;

use barq_common::secp256k1::PublicKey;
use barq_common::strategy::{Payee, RouteInput, StrategyKind};

use crate::methods::context;
use crate::methods::pay::{describe_path, strategy_name, RouteHopJson};
use crate::plugin::State;

/// Request payload for Barq route info RPC method
#[derive(Deserialize, Serialize)]
pub struct BarqRouteInfoRequest {
    pub dest_pubkey: String,
    pub amount_msat: u64,
    /// Final CLTV delta requested by the destination (defaults to 18).
    #[serde(default)]
    pub cltv: Option<u32>,
    /// The strategy to use for routing the payment
    #[serde(default)]
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
    pub strategy: String,
    pub amount_msat: u64,
    pub fee_msat: u64,
    pub route: Vec<RouteHopJson>,
}

/// Barq RPC method to get route information
pub fn barq_route_info(plugin: &mut Plugin<State>, request: Value) -> Result<Value, PluginError> {
    log::info!("barqrouteinfo called with request: {}", request);
    let request: BarqRouteInfoRequest = json::from_value(request).map_err(|err| error!("{err}"))?;
    let state = plugin.state.clone();
    plugin.state.block_on(route_info(state, request))
}

async fn route_info(state: State, request: BarqRouteInfoRequest) -> Result<Value, PluginError> {
    let state = &state;
    let dest = PublicKey::from_str(&request.dest_pubkey)
        .map_err(|_| error!("Invalid dest_pubkey `{}`", request.dest_pubkey))?;
    let node = context::load(state).await?;

    let kind = request.strategy().map_err(|e| error!("{e}"))?;
    let input = RouteInput {
        src_pubkey: node.our_id,
        payee: Payee::Clear {
            node_id: dest,
            route_hints: vec![],
            features: None,
        },
        amount_msat: request.amount_msat,
        final_cltv_delta: request.cltv.unwrap_or(18),
        first_hops: node.first_hops,
        expiry_time: None,
    };
    let output = context::route_with(state, kind, &input)?;
    let response = BarqRouteInfoResponse {
        strategy: strategy_name(kind).to_owned(),
        amount_msat: request.amount_msat,
        fee_msat: output.fee_msat(),
        route: describe_path(&output.path),
    };
    Ok(json::to_value(response)?)
}
