use crate::{methods::utils::graph::build_network_graph, plugin::State};
use barq_common::strategy::RouteInput;
use clightningrpc_plugin::{error, errors::PluginError, plugin::Plugin};
use serde::{Deserialize, Serialize};
use serde_json as json;

/// Response from 'sendpay' RPC command of core lightning
#[derive(Deserialize, Debug, Serialize)]
pub struct PaymentResponse {
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
    pub bolt11_invoice: String, // TODO: Add more fields as needed
}

/// Response payload for Barq pay RPC method
#[derive(Deserialize, Serialize)]
pub struct BarqPayResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<PaymentResponse>,
    pub message: String,
    // TODO: Add more fields as needed
}

/// information corresponding to a lightning pay request
#[derive(Deserialize, Debug)]
struct Bolt11 {
    payee: String,
    amount_msat: u64,
    payment_hash: String,
    min_final_cltv_expiry: u64,
    payment_secret: String,
}

/// information corresponding to current lightning node
#[derive(Debug, Deserialize)]
pub struct NodeInfo {
    pub id: String,
}

/// Barq RPC method to execute a payment
pub fn barq_pay(
    plugin: &mut Plugin<State>,
    request: json::Value,
) -> Result<json::Value, PluginError> {
    log::info!("barqpay called with request: {}", request);
    let request: BarqPayRequest = json::from_value(request).map_err(|err| error!("{err}"))?;

    // TODO: Implement the logic to execute a payment using Barq strategies
    let state = &plugin.state;
    let router = state.router();

    let b11: Bolt11 = state
        .call(
            "decodepay",
            serde_json::json!({
                "bolt11": request.bolt11_invoice
            }),
        )
        .map_err(|err| error!("Error calling CLN RPC method: {err}"))?;

    // // TODO: use CLN RPC to query any information needed
    let node_info: NodeInfo = state
        .call("getinfo", serde_json::json!({}))
        .map_err(|err| error!("Error calling CLN RPC method: {err}"))?;

    // Build the network graph from the plugin state
    let network_graph = build_network_graph(state)?;

    // TODO: Constrcut `RouteInput` from the request and CLN information gathered
    let input = RouteInput {
        src_pubkey: node_info.id.clone(),
        dest_pubkey: b11.payee.clone(),
        amount_msat: b11.amount_msat,
        cltv: b11.min_final_cltv_expiry,
        graph: network_graph,
    };

    // Execute the routing process
    let router_output = router.execute(&input);
    let response = match router_output {
        Ok(output) => {
            let sendpay_request: json::Value = serde_json::json!({
                "route": output.path,
                "payment_hash": b11.payment_hash,
                "payment_secret": b11.payment_secret
            });

            let sendpay_response: PaymentResponse = state
                .call("sendpay", sendpay_request)
                .map_err(|err| error!("Error calling sendpay method: {err}"))?;

            let waitsendpay_request: json::Value = serde_json::json!({
                "payment_hash": sendpay_response.payment_hash.clone()
            });

            let waitsendpay_response: PaymentResponse = state
                .call("waitsendpay", waitsendpay_request)
                .map_err(|err| error!("Error calling waitsendpay method: {err}"))?;

            // Construct the response from the output
            BarqPayResponse {
                response: Some(waitsendpay_response),
                message: "barqpay executed successfully".to_string(),
            }
        }
        Err(err) => BarqPayResponse {
            response: None,
            message: format!("barqpay execution failed: {}", err),
        },
    };

    Ok(json::to_value(response)?)
}
