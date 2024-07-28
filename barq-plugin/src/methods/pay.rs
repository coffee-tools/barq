use serde::{Deserialize, Serialize};
use serde_json as json;

use clightningrpc_plugin::{error, errors::PluginError, plugin::Plugin};

use barq_common::strategy::RouteInput;

use crate::{methods::utils::graph::build_network_graph, plugin::State};

/// Response from `sendpay` RPC command of Core Lightning
///
/// See: https://docs.corelightning.org/reference/lightning-sendpay#return-value
#[derive(Deserialize, Debug, Serialize)]
pub struct CLNSendpayResponse {
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

/// Status of the payment
#[derive(Deserialize, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Pending,
    Complete,
}

/// Request payload for Barq pay RPC method
#[derive(Deserialize, Serialize, Debug)]
pub struct BarqPayRequest {
    pub bolt11_invoice: String,
    #[serde(default)]
    pub amount_msat: Option<u64>
}

/// Response payload for Barq pay RPC method
#[derive(Deserialize, Serialize)]
pub struct BarqPayResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<CLNSendpayResponse>,
}

/// Bolt11 invoice information
#[derive(Deserialize, Debug)]
struct Bolt11 {
    payee: String,
    amount_msat: Option<u64>,
    payment_hash: String,
    min_final_cltv_expiry: u64,
    payment_secret: String,
}

/// Node information
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

    let amt_msat = match b11.amount_msat {
            Some(amount) => {
                amount
            },
            None => {
                request.amount_msat.expect("amount_msat parameter required")
            }
    };

    let node_info: NodeInfo = state
        .call("getinfo", serde_json::json!({}))
        .map_err(|err| error!("Error calling CLN RPC method: {err}"))?;

    // Build the network graph from the plugin state
    let network_graph = build_network_graph(state)?;

    let input = RouteInput {
        src_pubkey: node_info.id.clone(),
        dest_pubkey: b11.payee.clone(),
        amount_msat: amt_msat,
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

            let sendpay_response: CLNSendpayResponse = state
                .call("sendpay", sendpay_request)
                .map_err(|err| error!("Error calling sendpay method: {err}"))?;

            let waitsendpay_request: json::Value = serde_json::json!({
                "payment_hash": sendpay_response.payment_hash.clone()
            });

            let waitsendpay_response: CLNSendpayResponse = state
                .call("waitsendpay", waitsendpay_request)
                .map_err(|err| error!("Error calling waitsendpay method: {err}"))?;

            // Construct the response from the output
            BarqPayResponse {
                status: "success".to_string(),
                message: None,
                response: Some(waitsendpay_response),
            }
        }
        Err(err) => BarqPayResponse {
            status: "failure".to_string(),
            message: Some(format!("barqpay execution failed: {}", err)),
            response: None,
        },
    };

    Ok(json::to_value(response)?)
}
