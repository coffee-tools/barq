//! `barqpay`: rank the routes of every applicable strategy, try the best
//! one, and fall back to the next on failure (issues #10 and #16).
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json as json;

use clightningrpc_plugin::error;
use clightningrpc_plugin::errors::PluginError;
use clightningrpc_plugin::plugin::Plugin;

use barq_common::algorithms::direct::Direct;
use barq_common::algorithms::probabilistic::LDKRoutingStrategy;
use barq_common::history::PaymentRecord;
use barq_common::ldk::routing::router::Path;
use barq_common::network::NetworkContext;
use barq_common::onion::{build_onion_hops, locate_failure, FailurePoint, RecipientFields};
use barq_common::scid;
use barq_common::selection::Candidate;
use barq_common::strategy::{RouteInput, Strategy, StrategyKind};

use crate::methods::cln::{
    self, OnionHopJson, SendOnionFirstHop, SendOnionRequest, SendPayStatus, WaitSendPay,
};
use crate::methods::context::{self, NodeContext};
use crate::methods::invoice::{self, DecodedInvoice, InvoiceKind};
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
    /// Force a strategy; by default every applicable one is ranked.
    #[serde(default)]
    pub strategy: Option<String>,
    /// Refresh the graph from the rapid gossip sync server before routing.
    #[serde(default)]
    pub use_rapid_gossip_sync: bool,
    #[serde(default)]
    pub label: Option<String>,
    /// Maximum number of routes to try (defaults to `barq-max-attempts`).
    #[serde(default)]
    pub max_attempts: Option<u32>,
}

impl BarqPayRequest {
    pub fn strategy(&self) -> anyhow::Result<Option<StrategyKind>> {
        self.strategy
            .as_deref()
            .map(StrategyKind::from_str)
            .transpose()
    }
}

pub use barq_common::history::{AttemptRecord as AttemptJson, HopRecord as RouteHopJson};

fn attempt_of(candidate: &Candidate, status: &str) -> AttemptJson {
    AttemptJson {
        strategy: candidate.strategy.clone(),
        route: describe_path(&candidate.output.path),
        fee_msat: candidate.fee_msat(),
        status: status.to_owned(),
        error: None,
        erring_channel: None,
    }
}

/// Response payload for Barq pay RPC method
#[derive(Deserialize, Serialize)]
pub struct BarqPayResponse {
    /// The payment's id in barq's history (`barqinspect id=...`).
    pub id: u64,
    pub status: String,
    /// The strategy whose route completed the payment.
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
    /// Every route tried, in order; the last one completed.
    pub attempts: Vec<AttemptJson>,
    pub response: SendPayStatus,
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

    // Decode the invoice while loading our node, channels and gossip.
    let (invoice, node) = tokio::try_join!(
        invoice::decode(state, &request.invoice),
        context::load(state)
    )?;
    if let Some(invoice_network) = invoice.network {
        if invoice_network != node.network {
            return Err(error!(
                "Invoice network ({invoice_network}) does not match node network ({})",
                node.network
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

    // From here on the payment is recorded in barq's history, whatever
    // the outcome.
    let store = state.store()?;
    let invoice_type = match invoice.kind {
        InvoiceKind::Bolt11 => "bolt11",
        InvoiceKind::Bolt12 => "bolt12",
    };
    let mut record = PaymentRecord::new(
        &invoice.payment_hash,
        &request.invoice,
        invoice_type,
        invoice.destination.clone(),
        amount_msat,
        request.label.clone(),
    );
    store
        .create(&mut record)
        .map_err(|err| error!("cannot record the payment: {err:#}"))?;
    record.event(
        log::Level::Info,
        format!(
            "paying {amount_msat} msat to {} ({invoice_type} invoice {})",
            invoice.destination.as_deref().unwrap_or("a blinded path"),
            invoice.payment_hash
        ),
    );

    let result = attempt_payment(
        state,
        &ctx,
        &request,
        &invoice,
        &node,
        amount_msat,
        &mut record,
    )
    .await;
    let result = match result {
        Ok(mut response) => {
            response["id"] = json::json!(record.id);
            Ok(response)
        }
        Err(err) => {
            let mut value = json::to_value(&err).unwrap_or_default();
            let code = value["code"].as_i64().unwrap_or(-1) as i32;
            let message = value["message"].as_str().unwrap_or_default().to_owned();
            record.fail(code, &message);
            record.event(log::Level::Warn, format!("payment failed: {message}"));
            let mut data = value["data"].take();
            if !data.is_object() {
                data = json::json!({});
            }
            data["id"] = json::json!(record.id);
            Err(PluginError::new(code, &message, Some(data)))
        }
    };
    if let Err(err) = store.save(&record) {
        log::warn!("cannot save payment record {}: {err:#}", record.id);
    }
    result
}

/// Rank the routes, try them in order, and fall back on failure.
async fn attempt_payment(
    state: &State,
    ctx: &Arc<NetworkContext>,
    request: &BarqPayRequest,
    invoice: &DecodedInvoice,
    node: &NodeContext,
    amount_msat: u64,
    record: &mut PaymentRecord,
) -> Result<json::Value, PluginError> {
    if request.use_rapid_gossip_sync {
        let timestamp = ctx
            .sync_rapid_gossip()
            .await
            .map_err(|err| error!("rapid gossip sync failed: {err}"))?;
        record.event(
            log::Level::Info,
            format!("rapid gossip sync applied, timestamp {timestamp}"),
        );
    }

    if node.first_hops.is_empty() {
        return Err(error!("No usable channel to pay from"));
    }

    let forced = request.strategy().map_err(|e| error!("{e}"))?;
    let max_attempts = request.max_attempts.unwrap_or(state.max_attempts).max(1);
    let payment = Payment {
        state,
        ctx,
        request,
        invoice,
        node,
        amount_msat,
    };

    let mut excluded: Vec<u64> = vec![];
    let mut last_failure: Option<Failure> = None;

    for attempt in 1..=max_attempts {
        let input = RouteInput {
            src_pubkey: node.our_id,
            payee: invoice.payee.clone(),
            amount_msat,
            final_cltv_delta: invoice.final_cltv_delta,
            first_hops: node.first_hops.clone(),
            expiry_time: invoice.expiry_time,
            strategy: forced,
            excluded_channels: excluded.clone(),
        };
        let ranking = context::rank(state, &input)?;
        for rejected in &ranking.rejected {
            record.event(
                log::Level::Debug,
                format!(
                    "attempt {attempt}: strategy `{}` has no route: {}",
                    rejected.strategy, rejected.reason
                ),
            );
        }
        let Some(best) = ranking.best().cloned() else {
            if let Some(failure) = last_failure {
                // The previous attempt failed and no other route is left.
                record.event(log::Level::Info, "no other route left");
                return Err(failure.into_error(&record.attempts));
            }
            return Err(error!(
                "No route found to pay the invoice: {}",
                ranking.explain()
            ));
        };
        record.event(
            log::Level::Info,
            format!(
                "attempt {attempt}/{max_attempts}: `{}` route through {} ({} hop(s)), {} msat fee, {} candidate(s)",
                best.strategy,
                describe_path(&best.output.path)
                    .iter()
                    .map(|h| h.channel.as_str())
                    .collect::<Vec<_>>()
                    .join(" -> "),
                best.output.path.hops.len(),
                best.fee_msat(),
                ranking.candidates.len()
            ),
        );

        match payment.send(&best.output.path).await? {
            Outcome::Complete {
                status,
                first_hop_amount_msat,
            } => {
                ctx.payment_succeeded(&best.output.path);
                record.attempts.push(attempt_of(&best, "complete"));
                let amount_sent_msat = status.amount_sent_msat.unwrap_or(first_hop_amount_msat);
                record.complete(status.payment_preimage.clone(), amount_sent_msat);
                record.event(
                    log::Level::Info,
                    format!("complete, {amount_sent_msat} msat sent"),
                );
                let path = &best.output.path;
                let response = BarqPayResponse {
                    id: record.id,
                    status: status.status.clone(),
                    strategy: best.strategy.clone(),
                    invoice_type: match invoice.kind {
                        InvoiceKind::Bolt11 => "bolt11".to_owned(),
                        InvoiceKind::Bolt12 => "bolt12".to_owned(),
                    },
                    payment_hash: invoice.payment_hash.clone(),
                    payment_preimage: status.payment_preimage.clone(),
                    destination: invoice.destination.clone(),
                    amount_msat,
                    amount_sent_msat,
                    fee_msat: first_hop_amount_msat.saturating_sub(amount_msat),
                    route: describe_path(path),
                    blinded_hops: path.blinded_tail.as_ref().map_or(0, |t| t.hops.len()),
                    attempts: record.attempts.clone(),
                    response: status,
                };
                return Ok(json::to_value(response)?);
            }
            Outcome::Failed(failure) => {
                let mut tried = attempt_of(&best, "failed");
                tried.error = Some(failure.message.clone());
                tried.erring_channel = failure.retry_channel.map(scid::to_cln);
                record.attempts.push(tried);
                let Some(channel) = failure.retry_channel else {
                    // The destination (or its blinded path) failed the
                    // payment: another route will not help.
                    record.event(
                        log::Level::Warn,
                        format!(
                            "attempt {attempt} failed at the destination: {}",
                            failure.message
                        ),
                    );
                    return Err(failure.into_error(&record.attempts));
                };
                record.event(
                    log::Level::Warn,
                    format!(
                        "attempt {attempt} failed: {}; excluding channel {}",
                        failure.message,
                        scid::to_cln(channel)
                    ),
                );
                excluded.push(channel);
                last_failure = Some(failure);
            }
        }
    }
    let failure = last_failure.expect("at least one attempt ran");
    record.event(
        log::Level::Warn,
        format!("giving up after {max_attempts} attempt(s)"),
    );
    Err(failure.into_error(&record.attempts))
}

/// Everything needed to send one route.
struct Payment<'a> {
    state: &'a State,
    ctx: &'a Arc<NetworkContext>,
    request: &'a BarqPayRequest,
    invoice: &'a DecodedInvoice,
    node: &'a NodeContext,
    amount_msat: u64,
}

enum Outcome {
    Complete {
        status: SendPayStatus,
        first_hop_amount_msat: u64,
    },
    Failed(Failure),
}

/// A failed attempt, with what it tells us about the route.
struct Failure {
    code: i32,
    message: String,
    data: Option<json::Value>,
    /// The channel to avoid on the next attempt; `None` when the failure is
    /// final (the destination or its blinded path rejected the payment).
    retry_channel: Option<u64>,
}

impl Failure {
    fn into_error(self, attempts: &[AttemptJson]) -> PluginError {
        let mut data = self.data.unwrap_or_else(|| json::json!({}));
        if let Some(obj) = data.as_object_mut() {
            obj.insert(
                "attempts".to_owned(),
                json::to_value(attempts).unwrap_or_default(),
            );
        }
        PluginError::new(self.code, &self.message, Some(data))
    }
}

impl Payment<'_> {
    /// Build the onion for `path`, send it and wait for the result.
    async fn send(&self, path: &Path) -> Result<Outcome, PluginError> {
        let (state, invoice, node) = (self.state, self.invoice, self.node);
        // Core Lightning computes CLTVs relative to the next block.
        let onion = build_onion_hops(
            path,
            self.amount_msat,
            &RecipientFields {
                payment_secret: invoice.payment_secret,
                payment_metadata: invoice.payment_metadata.clone(),
            },
            node.blockheight + 1,
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
        let first_channel = node
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
                .saturating_sub(node.blockheight + 1),
        };

        let sent = cln::sendonion_raw(
            state,
            SendOnionRequest {
                onion: &created.onion,
                first_hop,
                payment_hash: &invoice.payment_hash,
                shared_secrets: &created.shared_secrets,
                amount_msat: self.amount_msat,
                destination: invoice.destination.as_deref(),
                invstring: Some(&self.request.invoice),
                label: self.request.label.as_deref(),
            },
        )
        .await;
        let sent = match sent {
            Ok(sent) => sent,
            // Our own channel refused the HTLC right away: try another one.
            Err(err) if err.code == PAY_TRY_OTHER_ROUTE => {
                log::warn!("sendonion failed locally: {}", err.message);
                return Ok(Outcome::Failed(Failure {
                    code: err.code,
                    message: err.message,
                    data: err.data,
                    retry_channel: Some(first.short_channel_id),
                }));
            }
            Err(err) => return Err(PluginError::new(err.code, &err.message, err.data)),
        };

        match cln::waitsendpay(state, &invoice.payment_hash, sent.groupid).await? {
            WaitSendPay::Complete(status) => Ok(Outcome::Complete {
                status,
                first_hop_amount_msat: onion.first_hop_amount_msat,
            }),
            WaitSendPay::Failed { failure, error } => {
                log::warn!(
                    "payment {} failed: {} ({:?})",
                    invoice.payment_hash,
                    error.message,
                    failure
                );
                let point = failure
                    .erring_index
                    .and_then(|index| locate_failure(path, index));
                if let Some(point) = &point {
                    log::info!("payment {} failed at {:?}", invoice.payment_hash, point);
                    if let Some(scid) = point.penalised_channel() {
                        self.ctx.payment_failed(path, scid);
                    }
                }
                let retry_channel = point.as_ref().and_then(|p| {
                    if p.at_recipient || p.in_blinded_path {
                        None
                    } else {
                        p.channel
                    }
                });
                let (code, data) = enrich_failure(error.code, error.data, point.as_ref());
                Ok(Outcome::Failed(Failure {
                    code,
                    message: error.message,
                    data,
                    retry_channel,
                }))
            }
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
