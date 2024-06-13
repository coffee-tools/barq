use clightningrpc::requests;
use serde::{Deserialize, Serialize};
use serde_json as json;
use serde_json::Value;

use clightningrpc_plugin::{error, errors::PluginError, plugin::Plugin};

use barq_common::strategy::RouteInput;

use crate::{methods::{pay, utils::graph::build_network_graph}, plugin::State};

#[derive(Deserialize, Debug, Serialize)]
struct SentPaymentResponse {
    message: String,
    id: u64,
    created_index: u64,
    payment_hash: String,
    groupid: u64,
    destination: String,
    amount_msat: u64,
    amount_sent_msat: u64,
    created_at: u64,
    status: Status,
}

#[derive(Deserialize, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Pending,
    Complete,
}


/// Request payload for Barq pay RPC method
#[derive(Deserialize, Serialize)]
pub struct BarqPayRequest {
    pub bolt11_invoice: String
    // TODO: Add more fields as needed
}

/// Response payload for Barq pay RPC method
#[derive(Deserialize, Serialize)]
pub struct BarqPayResponse {
    pub payResponse: SentPaymentResponse,
    pub message: String,
    // pub route: String,
    // TODO: Add more fields as needed
}

#[derive(Deserialize, Debug)]
struct Bolt11 {
    payee: String,
    amount_msat: u64,
    payment_hash: String,
    payment_secret: String
}

#[derive(Debug, Deserialize)]
struct NodeInfo {
    id: String
}

/// Barq RPC method to execute a payment
pub fn barq_pay(plugin: &mut Plugin<State>, request: Value) -> Result<Value, PluginError> {
    log::info!("barqpay called with request: {}", request);
    let request: BarqPayRequest = json::from_value(request).map_err(|err| error!("{err}"))?;

    // TODO: Implement the logic to execute a payment using Barq strategies
    let state = &plugin.state;
    let router = state.router();

    let b11 : Bolt11 = state
        .call("decodepay", serde_json::json!({
            "bolt11": request.bolt11_invoice
        }))
        .map_err(|err| error!("Error calling CLN RPC method: {err}"))?;

    // // TODO: use CLN RPC to query any information needed
    let node_info : NodeInfo = state
        .call("getinfo", serde_json::json!({}))
        .map_err(|err| error!("Error calling CLN RPC method: {err}"))?;

    // Build the network graph from the plugin state
    let network_graph = build_network_graph(state)?;

    // TODO: Constrcut `RouteInput` from the request and CLN information gathered
    let input = RouteInput {
        src_pubkey: node_info.id.clone(),
        dest_pubkey: b11.payee.clone(),
        amount_msat: b11.amount_msat,
        graph: network_graph,
    };

    // // // Execute the routing process
    let output = router.execute(&input);

    let payRequest: Value = serde_json::json!({
        "route": output.path,
        "payment_hash": b11.payment_hash,
        "payment_secret": b11.payment_secret
    });

    let payResponse: SentPaymentResponse = state
        .call("sendpay", payRequest)
        .map_err(|err| error!("Error calling CLN RPC method: {err}"))?;

    // Construct the response from the output
    let response = BarqPayResponse {
        payResponse: payResponse,
        message: "Payment executed successfully".to_string(),
        // route: output.path.join(" -> "),
    };
    Ok(json::to_value(response)?)
}
