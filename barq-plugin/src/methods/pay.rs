use serde::{Deserialize, Serialize};
use serde_json as json;

use clightningrpc_gossip_map::GossipMap;
use clightningrpc_plugin::error;
use clightningrpc_plugin::errors::PluginError;
use clightningrpc_plugin::plugin::Plugin;

use barq_common::strategy::{RouteInput, Router};

use crate::methods::utils::graph::build_network_graph;
use crate::plugin::State;

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
// If no value is provided for `amount_msat`, `None` is chosen as the default.
#[derive(Deserialize, Serialize, Debug)]
pub struct BarqPayRequest {
    pub bolt11_invoice: String,
    #[serde(default)]
    pub amount_msat: Option<u64>,
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

/// Response from `decodepay` RPC command of Core Lightning
///
/// See: https://docs.corelightning.org/reference/lightning-decodepay#return-value
#[derive(Deserialize, Debug)]
struct Bolt11 {
    /// The BIP173 name for the currency
    currency: String,
    payee: String,
    amount_msat: Option<u64>,
    payment_hash: String,
    min_final_cltv_expiry: u64,
    payment_secret: String, // FIXME: Should this be optional?
}

/// Response from `getinfo` RPC command of Core Lightning
///
/// See: https://docs.corelightning.org/reference/lightning-getinfo#return-value
#[derive(Debug, Deserialize)]
pub struct NodeInfo {
    pub id: String,
    /// Represents the type of network on the node are working
    pub network: String,
}

/// Barq RPC method to execute a payment
pub fn barq_pay(
    plugin: &mut Plugin<State>,
    request: json::Value,
) -> Result<json::Value, PluginError> {
    log::info!("barqpay called with request: {}", request);
    let request: BarqPayRequest = json::from_value(request).map_err(|err| error!("{err}"))?;

    let state = &plugin.state;
    let router = Router::default();

    let b11: Bolt11 = state
        .call(
            "decodepay",
            serde_json::json!({
                "bolt11": request.bolt11_invoice
            }),
        )
        .map_err(|err| error!("Error calling CLN RPC method: {err}"))?;

    let amt_msat = b11
        .amount_msat
        .or(request.amount_msat)
        .expect("amount_msat parameter required");

    // Get the network of the invoice
    // See: https://github.com/lightning/bolts/blob/master/11-payment-encoding.md#human-readable-part
    let invoice_network = match b11.currency.as_str() {
        "bc" => "bitcoin",
        "tb" => "testnet",
        "tbs" => "signet",
        "bcrt" => "regtest",
        _ => "unknown",
    };

    let node_info: NodeInfo = state
        .call("getinfo", serde_json::json!({}))
        .map_err(|err| error!("Error calling CLN RPC method: {err}"))?;

    let node_network = node_info.network.as_str();

    if invoice_network != node_network {
        return Err(error!(
            "Invoice network ({}) does not match node network ({})",
            invoice_network, node_network
        ));
    }

    // Get the gossip map path from the plugin state
    // FIXME: Currently, we are loading the gossip map from the file system
    //        each time the `barqpay` method is called. This is not efficient.
    //        We should load the gossip map once and cache it in the plugin state.
    //        See: https://github.com/tareknaser/barq/issues/21 for more details.

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
    let gossip_map_path = lightning_path.join(node_network).join("gossip_store");
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
