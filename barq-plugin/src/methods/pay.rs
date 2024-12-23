use std::str::FromStr;

use barq_common::algorithms::direct::Direct;
use barq_common::algorithms::probabilistic::LDKRoutingStrategy;
use serde::{Deserialize, Serialize};
use serde_json as json;

use clightningrpc_plugin::error;
use clightningrpc_plugin::errors::PluginError;
use clightningrpc_plugin::plugin::Plugin;

use barq_common::graph::NetworkGraph;
use barq_common::strategy::{RouteInput, Strategy, StrategyKind};
use barq_common::Network;

use crate::methods::graph::cln::build_cln_network_graph;
use crate::methods::graph::p2p::build_p2p_network_graph;
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
// serde(default) sets the default value to None if no input is given.
#[derive(Deserialize, Serialize)]
pub struct BarqPayRequest {
    pub bolt11_invoice: String,
    #[serde(default)]
    pub amount_msat: Option<u64>,
    /// The strategy to use for routing the payment
    #[serde(default)]
    pub strategy: Option<String>,
    /// Whether to use the rapid gossip sync map to build the network graph
    ///
    /// If false, we will try to use CLN gossip map to build the network
    /// graph
    #[serde(default)]
    pub use_rapid_gossip_sync: bool,
}

impl BarqPayRequest {
    pub fn strategy(&self) -> anyhow::Result<StrategyKind> {
        if let Some(ref s) = self.strategy {
            return StrategyKind::from_str(s);
        }
        Ok(StrategyKind::default())
    }
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

/// Decode response for Bolt11/Bolt12 invoices
#[derive(Deserialize, Debug)]
struct DecodedInvoice {
    /// The BIP173 name for the currency
    currency: Option<String>,
    payee: Option<String>,
    amount_msat: Option<u64>,
    payment_hash: String,
    min_final_cltv_expiry: Option<u64>,
    payment_secret: Option<String>,
    type_: Option<String>, // Indicates if it's Bolt11 or Bolt12
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

    // Decode the invoice using the `decode` command
    let decoded: DecodedInvoice = state
        .call(
            "decode",
            serde_json::json!({
                "string": request.bolt11_invoice
            }),
        )
        .map_err(|err| PluginError::new(err.code, &err.message, err.data))?;

    let invoice_type = decoded.type_.as_deref().unwrap_or("unknown");
    log::info!("Invoice type detected: {}", invoice_type);

    let currency = decoded.currency.ok_or_else(|| error!("Missing currency field"))?;
    let payee = decoded.payee.ok_or_else(|| error!("Missing payee field"))?;
    let amount = match (decoded.amount_msat, request.amount_msat) {
        (Some(_), Some(_)) => {
            return Err(error!("barqpay execution failed: amount_msat not required"))
        }
        (Some(amount), None) | (None, Some(amount)) => amount,
        (None, None) => return Err(error!("barqpay execution failed: amount_msat not required")),
    };

    let invoice_network = match currency.as_str() {
        "bc" => Network::Bitcoin,
        "tb" => Network::Testnet,
        "tbs" => Network::Signet,
        "bcrt" => Network::Regtest,
        _ => return Err(error!("Unknown currency: {}", currency)),
    };

    let node_info: NodeInfo = state
        .call("getinfo", serde_json::json!({}))
        .map_err(|err| PluginError::new(err.code, &err.message, err.data))?;
    let node_network = Network::from_str(&node_info.network).map_err(|e| error!("{e}"))?;

    if invoice_network != node_network {
        return Err(error!(
            "Invoice network ({}) does not match node network ({})",
            invoice_network, node_network
        ));
    }

    let strategy = request.strategy().map_err(|e| error!("{e}"))?;
    // If the probabilistic strategy is selected, build the network graph from the
    // gossip map. Else, build the network graph from the plugin state
    let network_graph: Box<dyn NetworkGraph> = match strategy {
        StrategyKind::Direct => Box::new(build_cln_network_graph(state)?),
        StrategyKind::Probabilistic => Box::new(build_p2p_network_graph(state)?),
    };

    let input = RouteInput {
        src_pubkey: node_info.id.clone(),
        dest_pubkey: payee,
        network: node_network,
        amount_msat: amount,
        cltv: decoded.min_final_cltv_expiry.unwrap_or_default(),
        graph: network_graph,
        use_rapid_gossip_sync: request.use_rapid_gossip_sync,
    };

    let strategy: Box<dyn Strategy> = match strategy {
        StrategyKind::Direct => Box::new(Direct::new()),
        StrategyKind::Probabilistic => Box::new(LDKRoutingStrategy::new(
            node_network,
            plugin.state.cln_rpc_path.clone().unwrap(),
        )),
    };

    // Execute the routing process
    let output = strategy.route(&input).map_err(|err| error!("{err}"))?;
    if output.path.is_empty() {
        return Err(error!("No route found between us and `{}`", payee));
    }

    log::info!("Path selected by the strategy: {:?}", output.path);

    let sendpay_request: json::Value = serde_json::json!({
        "route": output.path,
        "payment_hash": decoded.payment_hash,
        "payment_secret": decoded.payment_secret,
        "partid": 0,
    });

    let sendpay_response: CLNSendpayResponse = state
        .call("sendpay", sendpay_request)
        .map_err(|err| PluginError::new(err.code, &err.message, err.data))?;

    let waitsendpay_request: json::Value = serde_json::json!({
        "payment_hash": sendpay_response.payment_hash.clone(),
        "partid": 0,
    });

    let waitsendpay_response: CLNSendpayResponse =
        state
            .call("waitsendpay", waitsendpay_request)
            .map_err(|err| PluginError::new(err.code, &err.message, err.data))?;

    // Construct the response from the output
    let response = BarqPayResponse {
        status: "success".to_string(),
        message: None,
        response: Some(waitsendpay_response),
    };

    Ok(json::to_value(response)?)
}
