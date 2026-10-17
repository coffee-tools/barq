//! `barqpay`: route with the selected strategy, build the onion and let
//! Core Lightning send it.
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json as json;

use clightningrpc_plugin::error;
use clightningrpc_plugin::errors::PluginError;
use clightningrpc_plugin::plugin::Plugin;

use barq_common::algorithms::direct::Direct;
use barq_common::algorithms::probabilistic::LDKRoutingStrategy;
use barq_common::ldk::routing::router::Path;
use barq_common::onion::{build_onion_hops, locate_failure, FailurePoint, RecipientFields};
use barq_common::secp256k1::PublicKey;
use barq_common::strategy::{RouteInput, Strategy, StrategyKind};
use barq_common::{scid, Network};

use crate::methods::cln::{
    self, OnionHopJson, SendOnionFirstHop, SendOnionRequest, SendPayStatus, WaitSendPay,
};
use crate::methods::invoice::{self, InvoiceKind};
use crate::plugin::State;

/// Request payload for Barq pay RPC method
// serde(default) sets the default value to None if no input is given.
#[derive(Deserialize, Serialize)]
pub struct BarqPayRequest {
    /// A BOLT11 or BOLT12 invoice.
    #[serde(alias = "bolt11_invoice")]
    pub invoice: String,
    #[serde(default)]
    pub amount_msat: Option<u64>,
    /// The strategy to use for routing the payment
    #[serde(default)]
    pub strategy: Option<String>,
    /// Refresh the graph from the rapid gossip sync server before routing.
    #[serde(default)]
    pub use_rapid_gossip_sync: bool,
    #[serde(default)]
    pub label: Option<String>,
}

impl BarqPayRequest {
    pub fn strategy(&self) -> anyhow::Result<StrategyKind> {
        if let Some(ref s) = self.strategy {
            return StrategyKind::from_str(s);
        }
        Ok(StrategyKind::default())
    }
}

/// One hop of the route, in Core Lightning terms.
#[derive(Deserialize, Serialize)]
pub struct RouteHopJson {
    pub id: String,
    pub channel: String,
    pub fee_msat: u64,
    pub cltv_expiry_delta: u32,
}

/// Response payload for Barq pay RPC method
#[derive(Deserialize, Serialize)]
pub struct BarqPayResponse {
    pub status: String,
    pub strategy: String,
    pub invoice_type: String,
    pub payment_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_preimage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    pub amount_msat: u64,
    pub amount_sent_msat: u64,
    pub fee_msat: u64,
    pub route: Vec<RouteHopJson>,
    pub blinded_hops: usize,
    pub response: SendPayStatus,
}

pub fn strategy_name(kind: StrategyKind) -> &'static str {
    match kind {
        StrategyKind::Direct => "direct",
        StrategyKind::Probabilistic => "probabilistic",
    }
}

pub fn build_strategy(state: &State, kind: StrategyKind) -> Result<Box<dyn Strategy>, PluginError> {
    Ok(match kind {
        StrategyKind::Direct => Box::new(Direct::new()),
        StrategyKind::Probabilistic => Box::new(LDKRoutingStrategy::new(state.ctx()?)),
    })
}

/// Describe an LDK path with Core Lightning identifiers.
pub fn describe_path(path: &Path) -> Vec<RouteHopJson> {
    path.hops
        .iter()
        .map(|hop| RouteHopJson {
            id: hop.pubkey.to_string(),
            channel: scid::to_cln(hop.short_channel_id),
            fee_msat: hop.fee_msat,
            cltv_expiry_delta: hop.cltv_expiry_delta,
        })
        .collect()
}

/// Barq RPC method to execute a payment.
///
/// cln4rust dispatches RPC methods synchronously, so the async payment flow
/// runs to completion on the plugin's tokio runtime.
pub fn barq_pay(
    plugin: &mut Plugin<State>,
    request: json::Value,
) -> Result<json::Value, PluginError> {
    log::info!("barqpay called with request: {}", request);
    let request: BarqPayRequest = json::from_value(request).map_err(|err| error!("{err}"))?;
    let state = plugin.state.clone();
    plugin.state.block_on(pay(state, request))
}

async fn pay(state: State, request: BarqPayRequest) -> Result<json::Value, PluginError> {
    let state = &state;
    let ctx = state.ctx()?;

    // These are independent: decode the invoice, ask for our node info and
    // channels, and catch up with the gossip Core Lightning received since
    // the last background sync, all at the same time.
    let gossip_store = async {
        if ctx.gossip_store_path().is_some() {
            match ctx.sync_gossip_store_async().await {
                Ok(stats) => log::debug!("gossip_store sync before routing: {stats:?}"),
                Err(err) => log::warn!("gossip_store sync failed: {err}"),
            }
        }
        Ok::<(), PluginError>(())
    };
    let (invoice, node_info, first_hops, ()) = tokio::try_join!(
        invoice::decode(state, &request.invoice),
        cln::getinfo(state),
        cln::first_hops(state),
        gossip_store,
    )?;

    let our_id = PublicKey::from_str(&node_info.id).map_err(|_| error!("Invalid node id"))?;
    let node_network = Network::from_str(&node_info.network).map_err(|e| error!("{e}"))?;
    if let Some(invoice_network) = invoice.network {
        if invoice_network != node_network {
            return Err(error!(
                "Invoice network ({invoice_network}) does not match node network ({node_network})"
            ));
        }
    }

    let amount_msat = match (invoice.amount_msat, request.amount_msat) {
        (Some(_), Some(_)) => {
            return Err(error!(
                "barqpay execution failed: amount_msat not required, the invoice has an amount"
            ))
        }
        (Some(amount), None) | (None, Some(amount)) => amount,
        (None, None) => {
            return Err(error!(
                "barqpay execution failed: amount_msat required for an invoice without amount"
            ))
        }
    };
    if amount_msat == 0 {
        return Err(error!(
            "barqpay execution failed: amount_msat must be positive"
        ));
    }

    if request.use_rapid_gossip_sync {
        ctx.sync_rapid_gossip()
            .await
            .map_err(|err| error!("rapid gossip sync failed: {err}"))?;
    }

    if first_hops.is_empty() {
        return Err(error!("No usable channel to pay from"));
    }

    let kind = request.strategy().map_err(|e| error!("{e}"))?;
    let strategy = build_strategy(state, kind)?;
    let input = RouteInput {
        src_pubkey: our_id,
        payee: invoice.payee.clone(),
        amount_msat,
        final_cltv_delta: invoice.final_cltv_delta,
        first_hops,
        expiry_time: invoice.expiry_time,
    };
    if !strategy.can_apply(&input).map_err(|e| error!("{e}"))? {
        return Err(error!(
            "Strategy `{}` cannot pay this invoice",
            strategy_name(kind)
        ));
    }
    let output = strategy.route(&input).map_err(|err| error!("{err}"))?;
    let path = output.path;
    if path.hops.is_empty() {
        return Err(error!("No route found to pay the invoice"));
    }
    log::info!("path selected by `{}`: {:?}", strategy_name(kind), path);

    // Core Lightning computes CLTVs relative to the next block.
    let onion = build_onion_hops(
        &path,
        amount_msat,
        &RecipientFields {
            payment_secret: invoice.payment_secret,
            payment_metadata: invoice.payment_metadata.clone(),
        },
        node_info.blockheight + 1,
    )
    .map_err(|err| error!("cannot build the onion: {err}"))?;

    let hops: Vec<OnionHopJson> = onion
        .hops
        .iter()
        .map(|hop| OnionHopJson {
            pubkey: hop.pubkey.to_string(),
            payload: hex::encode(&hop.payload),
        })
        .collect();
    let created = cln::createonion(state, &hops, &invoice.payment_hash).await?;

    let first = &path.hops[0];
    let first_channel = input
        .first_hops
        .iter()
        .find(|hop| hop.scid == first.short_channel_id && hop.peer_id == first.pubkey)
        .map(|hop| hop.cln_channel.clone())
        .unwrap_or_else(|| scid::to_cln(first.short_channel_id));
    let first_hop = SendOnionFirstHop {
        id: first.pubkey.to_string(),
        channel: first_channel,
        amount_msat: onion.first_hop_amount_msat,
        delay: onion
            .first_hop_cltv_expiry
            .saturating_sub(node_info.blockheight + 1),
    };

    let sent = cln::sendonion(
        state,
        SendOnionRequest {
            onion: &created.onion,
            first_hop,
            payment_hash: &invoice.payment_hash,
            shared_secrets: &created.shared_secrets,
            amount_msat,
            destination: invoice.destination.as_deref(),
            invstring: Some(&request.invoice),
            label: request.label.as_deref(),
        },
    )
    .await?;

    match cln::waitsendpay(state, &invoice.payment_hash, sent.groupid).await? {
        WaitSendPay::Complete(status) => {
            ctx.payment_succeeded(&path);
            let response = BarqPayResponse {
                status: status.status.clone(),
                strategy: strategy_name(kind).to_owned(),
                invoice_type: match invoice.kind {
                    InvoiceKind::Bolt11 => "bolt11".to_owned(),
                    InvoiceKind::Bolt12 => "bolt12".to_owned(),
                },
                payment_hash: invoice.payment_hash.clone(),
                payment_preimage: status.payment_preimage.clone(),
                destination: invoice.destination.clone(),
                amount_msat,
                amount_sent_msat: status
                    .amount_sent_msat
                    .unwrap_or(onion.first_hop_amount_msat),
                fee_msat: onion.first_hop_amount_msat.saturating_sub(amount_msat),
                route: describe_path(&path),
                blinded_hops: path.blinded_tail.as_ref().map_or(0, |t| t.hops.len()),
                response: status,
            };
            Ok(json::to_value(response)?)
        }
        WaitSendPay::Failed { failure, error } => {
            log::warn!(
                "payment {} failed: {} ({:?})",
                invoice.payment_hash,
                error.message,
                failure
            );
            let point = failure
                .erring_index
                .and_then(|index| locate_failure(&path, index));
            if let Some(point) = &point {
                log::info!("payment {} failed at {:?}", invoice.payment_hash, point);
                if let Some(scid) = point.penalised_channel() {
                    ctx.payment_failed(&path, scid);
                }
            }
            let (code, data) = enrich_failure(error.code, error.data, point.as_ref());
            Err(PluginError::new(code, &error.message, data))
        }
    }
}

/// `waitsendpay` error code for a failure along the route.
const PAY_TRY_OTHER_ROUTE: i32 = 204;
/// `waitsendpay` error code for a permanent failure at the destination.
const PAY_DESTINATION_PERM_FAIL: i32 = 203;

/// Fill in what Core Lightning cannot know after `sendonion`: the failing
/// node and channel, and whether the destination itself rejected the
/// payment (reported as 203 instead of 204, like `sendpay` does).
fn enrich_failure(
    code: i32,
    data: Option<json::Value>,
    point: Option<&FailurePoint>,
) -> (i32, Option<json::Value>) {
    let Some(point) = point else {
        return (code, data);
    };
    let mut data = data.unwrap_or_else(|| json::json!({}));
    if let Some(obj) = data.as_object_mut() {
        if let Some(node) = point.node {
            obj.entry("erring_node")
                .or_insert_with(|| json::Value::String(node.to_string()));
        }
        if let Some(channel) = point.channel {
            obj.entry("erring_channel")
                .or_insert_with(|| json::Value::String(scid::to_cln(channel)));
        }
        obj.insert(
            "erring_in_blinded_path".to_owned(),
            json::Value::Bool(point.in_blinded_path),
        );
    }
    let code = if code == PAY_TRY_OTHER_ROUTE && point.at_recipient && !point.in_blinded_path {
        PAY_DESTINATION_PERM_FAIL
    } else {
        code
    };
    (code, Some(data))
}
