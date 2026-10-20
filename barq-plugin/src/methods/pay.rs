//! `barqpay`: rank the routes of every applicable strategy, try the best
//! one, and fall back to the next on failure (issues #10 and #16).
use std::str::FromStr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json as json;

use clightningrpc_plugin::error;
use clightningrpc_plugin::errors::PluginError;
use clightningrpc_plugin::plugin::Plugin;

use barq_common::algorithms::direct::Direct;
use barq_common::algorithms::probabilistic::LDKRoutingStrategy;
use barq_common::bitcoin::hashes::{sha256, Hash};
use barq_common::failure::{
    classify_failure, parse_failure_message, reported_height, CarriedUpdate, FailureKind,
    INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS,
};
use barq_common::history::PaymentRecord;
use barq_common::ldk::blinded_path::{BlindedPath, Direction, IntroductionNode};
use barq_common::ldk::offers::invoice::BlindedPayInfo;
use barq_common::ldk::routing::router::Path;
use barq_common::network::NetworkContext;
use barq_common::onion::{
    build_onion_hops, locate_failure, path_from_introduction, FailurePoint, RecipientFields,
};
use barq_common::scid;
use barq_common::secp256k1::PublicKey;
use barq_common::selection::{Candidate, Ranking};
use barq_common::strategy::{Payee, RouteInput, Strategy, StrategyKind};

use crate::methods::cln::{
    self, InjectRequest, OnionHopJson, Part, SelfPayRequest, SendOnionFirstHop, SendOnionRequest,
    SendPayStatus, WaitSendPay,
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
    /// The most the payment may pay in fees. When unset, `maxfeepercent`
    /// and `exemptfee_msat` set the limit; with neither there is none.
    #[serde(default)]
    pub maxfee_msat: Option<u64>,
    /// Fee limit as a percentage of the amount, like `pay`'s
    /// `maxfeepercent`.
    #[serde(default)]
    pub maxfeepercent: Option<f64>,
    /// Fees up to this amount are always accepted, like `pay`'s
    /// `exemptfee`.
    #[serde(default)]
    pub exemptfee_msat: Option<u64>,
    /// Nodes (by id) and channels (`scid` or `scid/direction`) to avoid.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Pay only this part of the invoice amount: other payers send the
    /// rest (`pay`'s `partial_msat`).
    #[serde(default)]
    pub partial_msat: Option<u64>,
    /// The description a `description_hash` invoice commits to.
    #[serde(default)]
    pub description: Option<String>,
}

impl BarqPayRequest {
    pub fn strategy(&self) -> anyhow::Result<Option<StrategyKind>> {
        self.strategy
            .as_deref()
            .map(StrategyKind::from_str)
            .transpose()
    }

    /// The most this payment may pay in fees for `amount_msat`.
    pub fn fee_budget(&self, amount_msat: u64) -> Option<u64> {
        if let Some(maxfee) = self.maxfee_msat {
            return Some(maxfee);
        }
        let percent = self.maxfeepercent?;
        let by_percent = (amount_msat as f64 * percent / 100.0).floor() as u64;
        Some(by_percent.max(self.exemptfee_msat.unwrap_or(0)))
    }
}

/// `pay` error codes (see `lightning-pay(7)`).
pub const PAY_IN_PROGRESS: i32 = 200;
pub const PAY_DESTINATION_PERM_FAIL: i32 = 203;
/// `waitsendpay` error code for a failure along the route.
pub const PAY_TRY_OTHER_ROUTE: i32 = 204;
pub const PAY_ROUTE_NOT_FOUND: i32 = 205;
pub const PAY_ROUTE_TOO_EXPENSIVE: i32 = 206;
pub const PAY_INVOICE_EXPIRED: i32 = 207;
const JSONRPC2_INVALID_PARAMS: i32 = -32602;

fn fail(code: i32, message: impl AsRef<str>) -> PluginError {
    PluginError::new(code, message.as_ref(), None)
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
    /// How many attempts this took, like `pay`'s `parts`.
    pub parts: usize,
    pub response: SendPayStatus,
}

pub fn build_strategy(state: &State, kind: StrategyKind) -> Result<Box<dyn Strategy>, PluginError> {
    Ok(match kind {
        StrategyKind::Direct => Box::new(Direct::new()),
        StrategyKind::Probabilistic => Box::new(LDKRoutingStrategy::new(state.ctx()?)),
        StrategyKind::External => Box::new(state.external_strategy.clone().ok_or_else(|| {
            error!("No external strategy configured: set `barq-external-strategy`")
        })?),
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

pub(crate) async fn pay(state: State, request: BarqPayRequest) -> Result<json::Value, PluginError> {
    let state = &state;
    let ctx = state.ctx()?;

    // Decode the invoice while loading our node, channels, gossip and the
    // chain's header height.
    let (invoice, node, headers) = tokio::try_join!(
        invoice::decode(state, &request.invoice),
        context::load(state),
        async { Ok::<_, PluginError>(cln::headercount(state).await) }
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
    let send_msat = match request.partial_msat {
        None => amount_msat,
        Some(partial) if partial == 0 || partial > amount_msat => {
            return Err(fail(
                JSONRPC2_INVALID_PARAMS,
                format!(
                "partial_msat must be less or equal to total amount {amount_msat}msat and positive"
            ),
            ))
        }
        Some(partial) => partial,
    };
    if let (Some(hash), Some(description)) = (&invoice.description_hash, &request.description) {
        let digest = sha256::Hash::hash(description.as_bytes()).to_string();
        if digest != *hash {
            return Err(fail(
                JSONRPC2_INVALID_PARAMS,
                format!("description_hash {hash} does not match description (hash {digest})"),
            ));
        }
    }
    if let Some(expiry) = invoice.expiry_time {
        if now_secs() > expiry {
            return Err(fail(PAY_INVOICE_EXPIRED, "Invoice expired"));
        }
    }

    // Like `pay`: an invoice already paid answers with that payment, and
    // a payment still in flight is not started twice. Every attempt of
    // this payment goes in the next group.
    let previous = cln::listsendpays(state, &invoice.payment_hash).await?;
    if let Some(done) = previous.iter().find(|p| p.status == "complete") {
        log::info!("invoice {} is already paid", invoice.payment_hash);
        return Ok(already_paid(&invoice, amount_msat, &previous, done));
    }
    if let Some(pending) = previous.iter().find(|p| p.status == "pending") {
        return Err(fail(
            PAY_IN_PROGRESS,
            format!(
                "Payment with groupid={} still in progress, cannot retry before that completes.",
                pending.groupid.unwrap_or(0)
            ),
        ));
    }
    let groupid = previous
        .iter()
        .filter_map(|p| p.groupid)
        .max()
        .map_or(1, |g| g + 1);
    // CLTVs follow the chain's headers when we are still catching up.
    let height = node.blockheight.max(headers.unwrap_or(0));

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
            "paying {} msat to {} ({invoice_type} invoice {})",
            if send_msat == amount_msat {
                amount_msat.to_string()
            } else {
                format!("{send_msat} of {amount_msat}")
            },
            invoice.destination.as_deref().unwrap_or("a blinded path"),
            invoice.payment_hash
        ),
    );

    let payment = Payment {
        state,
        ctx: &ctx,
        request: &request,
        invoice: &invoice,
        node: &node,
        amount_msat,
        send_msat,
        groupid,
    };
    let result = payment.run(height, &mut record).await;
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The answer for an invoice paid earlier: that payment, like `pay` does.
fn already_paid(
    invoice: &DecodedInvoice,
    amount_msat: u64,
    previous: &[SendPayStatus],
    done: &SendPayStatus,
) -> json::Value {
    let group: Vec<&SendPayStatus> = previous
        .iter()
        .filter(|p| p.groupid == done.groupid)
        .collect();
    let sent: u64 = group
        .iter()
        .filter(|p| p.status == "complete")
        .filter_map(|p| p.amount_sent_msat)
        .sum();
    json::json!({
        "status": "complete",
        "strategy": "none",
        "already_paid": true,
        "invoice_type": match invoice.kind {
            InvoiceKind::Bolt11 => "bolt11",
            InvoiceKind::Bolt12 => "bolt12",
        },
        "payment_hash": invoice.payment_hash,
        "payment_preimage": done.payment_preimage,
        "destination": invoice.destination,
        "amount_msat": amount_msat,
        "amount_sent_msat": sent,
        "fee_msat": sent.saturating_sub(amount_msat),
        "route": [],
        "blinded_hops": 0,
        "attempts": [],
        "parts": group.len(),
        "response": done,
    })
}

/// Everything needed to pay one invoice.
struct Payment<'a> {
    state: &'a State,
    ctx: &'a Arc<NetworkContext>,
    request: &'a BarqPayRequest,
    invoice: &'a DecodedInvoice,
    node: &'a NodeContext,
    /// What the invoice asks for, in total.
    amount_msat: u64,
    /// What this payment delivers (less than `amount_msat` for a partial
    /// payment).
    send_msat: u64,
    groupid: u64,
}

enum Outcome {
    Complete {
        status: SendPayStatus,
        first_hop_amount_msat: u64,
    },
    Failed(Failure),
}

/// What the next attempt should do after a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retry {
    /// The destination (or its blinded path) rejected the payment: another
    /// route will not help.
    Final,
    /// Avoid this channel.
    Exclude(u64),
    /// The node sent its current policy for this channel, now in the
    /// graph: the channel is worth another try.
    Policy(u64),
    /// The recipient is at this (higher) block height: try again with CLTVs
    /// computed from it.
    Height(u32),
}

/// A failed attempt, with what it tells us about the route.
struct Failure {
    code: i32,
    message: String,
    data: Option<json::Value>,
    channel: Option<u64>,
    retry: Retry,
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

    /// No route left after `attempts`: report it like `pay` does.
    fn give_up(mut self, attempts: &[AttemptJson]) -> PluginError {
        self.message = format!("Failed after {} attempts. {}", attempts.len(), self.message);
        self.into_error(attempts)
    }
}

impl Payment<'_> {
    /// Pay: to ourselves, through a blinded path we introduce, or along
    /// ranked routes with fallback.
    async fn run(
        &self,
        height: u32,
        record: &mut PaymentRecord,
    ) -> Result<json::Value, PluginError> {
        let (request, invoice, node) = (self.request, self.invoice, self.node);
        if request.use_rapid_gossip_sync {
            let timestamp = self
                .ctx
                .sync_rapid_gossip()
                .await
                .map_err(|err| error!("rapid gossip sync failed: {err}"))?;
            record.event(
                log::Level::Info,
                format!("rapid gossip sync applied, timestamp {timestamp}"),
            );
        }
        // A BOLT11 invoice of ours is paid with `sendpay` like `pay` does;
        // a BOLT12 one reaches us through its blinded path, below.
        if self.to_ourselves() && invoice.kind == InvoiceKind::Bolt11 {
            return self.pay_ourselves(record).await;
        }
        if let Some((payinfo, path)) = introduced_by_us(invoice, node) {
            return self
                .pay_as_introduction(payinfo, path, height, record)
                .await;
        }
        if node.first_hops.is_empty() {
            return Err(fail(PAY_ROUTE_NOT_FOUND, "No usable channel to pay from"));
        }

        let forced = request.strategy().map_err(|e| error!("{e}"))?;
        let max_attempts = request
            .max_attempts
            .unwrap_or(self.state.max_attempts)
            .max(1);
        let budget = request.fee_budget(self.send_msat);
        let mut excluded = self.exclusions()?;
        let mut height = height;
        let mut policy_retried: Vec<u64> = vec![];
        // Like xpay, a recipient ahead of our chain view earns one retry:
        // on a slow node every attempt can see a newer block, and the
        // failure would otherwise be retried until the attempts run out.
        let mut height_retried = false;
        let mut last_failure: Option<Failure> = None;

        for attempt in 1..=max_attempts {
            let input = RouteInput {
                src_pubkey: node.our_id,
                payee: invoice.payee.clone(),
                amount_msat: self.send_msat,
                final_cltv_delta: invoice.final_cltv_delta,
                first_hops: node.first_hops.clone(),
                expiry_time: invoice.expiry_time,
                strategy: forced,
                excluded_channels: excluded.clone(),
            };
            let mut ranking = context::rank(self.state, &input)?;
            for rejected in &ranking.rejected {
                record.event(
                    log::Level::Debug,
                    format!(
                        "attempt {attempt}: strategy `{}` has no route: {}",
                        rejected.strategy, rejected.reason
                    ),
                );
            }
            if let Some(budget) = budget {
                let cheapest = ranking.candidates.iter().map(Candidate::fee_msat).min();
                ranking.candidates.retain(|c| c.fee_msat() <= budget);
                if let (Some(cheapest), None, None) =
                    (cheapest, ranking.best(), last_failure.as_ref())
                {
                    return Err(fail(
                        PAY_ROUTE_TOO_EXPENSIVE,
                        format!(
                            "Failed: Could not find route without excessive cost: the cheapest \
                             route costs {cheapest} msat in fees, the limit is {budget} msat"
                        ),
                    ));
                }
            }
            let Some(best) = ranking.best().cloned() else {
                if let Some(failure) = last_failure {
                    record.event(log::Level::Info, "no other route left");
                    return Err(failure.give_up(&record.attempts));
                }
                return Err(self.no_route(&ranking));
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

            // Every attempt is the next part of the same group, like `pay`.
            let part = Part {
                groupid: self.groupid,
                partid: attempt as u64,
            };
            match self
                .send(&best.output.path, part, height, !height_retried)
                .await?
            {
                Outcome::Complete {
                    status,
                    first_hop_amount_msat,
                } => {
                    self.ctx.payment_succeeded(&best.output.path);
                    record.attempts.push(attempt_of(&best, "complete"));
                    return Ok(self.completed(
                        record,
                        &best.strategy,
                        &best.output.path,
                        status,
                        first_hop_amount_msat,
                    ));
                }
                Outcome::Failed(failure) => {
                    let mut tried = attempt_of(&best, "failed");
                    tried.error = Some(failure.message.clone());
                    tried.erring_channel = failure.channel.map(scid::to_cln);
                    record.attempts.push(tried);
                    match failure.retry {
                        Retry::Final => {
                            record.event(
                                log::Level::Warn,
                                format!(
                                    "attempt {attempt} failed at the destination: {}",
                                    failure.message
                                ),
                            );
                            return Err(failure.into_error(&record.attempts));
                        }
                        Retry::Exclude(channel) => {
                            record.event(
                                log::Level::Warn,
                                format!(
                                    "attempt {attempt} failed: {}; excluding channel {}",
                                    failure.message,
                                    scid::to_cln(channel)
                                ),
                            );
                            excluded.push(channel);
                        }
                        Retry::Policy(channel) => {
                            // A second policy failure on the same channel
                            // means the update did not help.
                            if policy_retried.contains(&channel) {
                                excluded.push(channel);
                            } else {
                                policy_retried.push(channel);
                            }
                            record.event(
                                log::Level::Warn,
                                format!(
                                    "attempt {attempt} failed: {}; applied the channel_update for {}",
                                    failure.message,
                                    scid::to_cln(channel)
                                ),
                            );
                        }
                        Retry::Height(reported) => {
                            record.event(
                                log::Level::Warn,
                                format!(
                                    "attempt {attempt} failed: the destination is at block {reported}, \
                                     we used {height}: retrying from its height"
                                ),
                            );
                            height = reported;
                            height_retried = true;
                        }
                    }
                    last_failure = Some(failure);
                }
            }
        }
        let failure = last_failure.expect("at least one attempt ran");
        record.event(
            log::Level::Warn,
            format!("giving up after {max_attempts} attempt(s)"),
        );
        Err(failure.give_up(&record.attempts))
    }

    fn to_ourselves(&self) -> bool {
        self.invoice
            .destination
            .as_deref()
            .and_then(|d| PublicKey::from_str(d).ok())
            == Some(self.node.our_id)
    }

    /// No strategy found a route on the first attempt.
    fn no_route(&self, ranking: &Ranking) -> PluginError {
        fail(
            PAY_ROUTE_NOT_FOUND,
            format!(
                "No route found to pay the invoice: {} is not reachable directly and all \
                 routehints were unusable ({})",
                self.invoice
                    .destination
                    .as_deref()
                    .unwrap_or("the blinded path"),
                ranking.explain()
            ),
        )
    }

    /// The channels `exclude` rules out: the listed channels, and every
    /// channel of the listed nodes.
    fn exclusions(&self) -> Result<Vec<u64>, PluginError> {
        let mut channels = vec![];
        for item in &self.request.exclude {
            if let Ok(id) = PublicKey::from_str(item) {
                if id == self.node.our_id {
                    return Err(fail(PAY_ROUTE_NOT_FOUND, "Payer is manually excluded"));
                }
                if self
                    .invoice
                    .destination
                    .as_deref()
                    .and_then(|d| PublicKey::from_str(d).ok())
                    == Some(id)
                {
                    return Err(fail(PAY_ROUTE_NOT_FOUND, "Payee is manually excluded"));
                }
                channels.extend(self.ctx.channels_of(&id));
                channels.extend(
                    self.node
                        .first_hops
                        .iter()
                        .filter(|hop| hop.peer_id == id)
                        .map(|hop| hop.scid),
                );
                continue;
            }
            let channel = item.split('/').next().unwrap_or_default();
            let channel = scid::from_cln(channel).map_err(|_| {
                fail(
                    JSONRPC2_INVALID_PARAMS,
                    format!("Invalid exclude `{item}`: expected a node id or a short channel id"),
                )
            })?;
            channels.push(channel);
        }
        Ok(channels)
    }

    /// The successful answer, recorded in the history.
    fn completed(
        &self,
        record: &mut PaymentRecord,
        strategy: &str,
        path: &Path,
        status: SendPayStatus,
        first_hop_amount_msat: u64,
    ) -> json::Value {
        let amount_sent_msat = status.amount_sent_msat.unwrap_or(first_hop_amount_msat);
        record.complete(status.payment_preimage.clone(), amount_sent_msat);
        record.event(
            log::Level::Info,
            format!("complete, {amount_sent_msat} msat sent"),
        );
        let response = BarqPayResponse {
            id: record.id,
            status: status.status.clone(),
            strategy: strategy.to_owned(),
            invoice_type: match self.invoice.kind {
                InvoiceKind::Bolt11 => "bolt11".to_owned(),
                InvoiceKind::Bolt12 => "bolt12".to_owned(),
            },
            payment_hash: self.invoice.payment_hash.clone(),
            payment_preimage: status.payment_preimage.clone(),
            destination: self.invoice.destination.clone(),
            amount_msat: self.send_msat,
            amount_sent_msat,
            fee_msat: first_hop_amount_msat.saturating_sub(self.send_msat),
            route: describe_path(path),
            blinded_hops: path.blinded_tail.as_ref().map_or(0, |t| t.hops.len()),
            attempts: record.attempts.clone(),
            parts: record.attempts.len(),
            response: status,
        };
        json::to_value(response).unwrap_or_default()
    }

    /// A payment to ourselves: `sendpay` with an empty route, like `pay`.
    async fn pay_ourselves(&self, record: &mut PaymentRecord) -> Result<json::Value, PluginError> {
        record.event(log::Level::Info, "paying ourselves");
        let invoice = self.invoice;
        let outcome = cln::selfpay(
            self.state,
            SelfPayRequest {
                payment_hash: &invoice.payment_hash,
                amount_msat: self.send_msat,
                payment_secret: invoice.payment_secret,
                payment_metadata: invoice.payment_metadata.as_deref(),
                invstring: &self.request.invoice,
                label: self.request.label.as_deref(),
                description: self.request.description.as_deref(),
                part: Part {
                    groupid: self.groupid,
                    partid: 0,
                },
            },
        )
        .await?;
        let empty = Path {
            hops: vec![],
            blinded_tail: None,
        };
        self.finish_single("self", &empty, outcome, self.send_msat, record)
    }

    /// We are the introduction node of the recipient's blinded path: build
    /// the onion from our own node and inject it, like xpay.
    async fn pay_as_introduction(
        &self,
        payinfo: &BlindedPayInfo,
        blinded: &BlindedPath,
        height: u32,
        record: &mut PaymentRecord,
    ) -> Result<json::Value, PluginError> {
        record.event(
            log::Level::Info,
            "we are the introduction node of the blinded path: injecting the onion",
        );
        let invoice = self.invoice;
        let path = path_from_introduction(self.node.our_id, payinfo, blinded, self.send_msat);
        let onion = build_onion_hops(
            &path,
            self.amount_msat,
            &RecipientFields::default(),
            height + 1,
        )
        .map_err(|err| error!("cannot build the onion: {err}"))?;
        let created =
            cln::createonion(self.state, &onion_json(&onion), &invoice.payment_hash).await?;
        let outcome = cln::injectpaymentonion(
            self.state,
            InjectRequest {
                onion: &created.onion,
                payment_hash: &invoice.payment_hash,
                amount_msat: onion.first_hop_amount_msat,
                cltv_expiry: onion.first_hop_cltv_expiry,
                destination_msat: self.send_msat,
                part: Part {
                    groupid: self.groupid,
                    partid: 1,
                },
                invstring: &self.request.invoice,
                label: self.request.label.as_deref(),
                destination: invoice.destination.as_deref(),
            },
        )
        .await?;
        self.finish_single(
            "introduction",
            &path,
            outcome,
            onion.first_hop_amount_msat,
            record,
        )
    }

    /// The result of a payment sent in one go, without fallback.
    fn finish_single(
        &self,
        strategy: &str,
        path: &Path,
        outcome: WaitSendPay,
        sent_msat: u64,
        record: &mut PaymentRecord,
    ) -> Result<json::Value, PluginError> {
        let mut attempt = AttemptJson {
            strategy: strategy.to_owned(),
            route: describe_path(path),
            fee_msat: sent_msat.saturating_sub(self.send_msat),
            status: "complete".to_owned(),
            error: None,
            erring_channel: None,
        };
        match outcome {
            WaitSendPay::Complete(status) => {
                record.attempts.push(attempt);
                Ok(self.completed(record, strategy, path, status, sent_msat))
            }
            WaitSendPay::Failed { error, .. } => {
                attempt.status = "failed".to_owned();
                attempt.error = Some(error.message.clone());
                record.attempts.push(attempt);
                Err(Failure {
                    code: error.code,
                    message: error.message,
                    data: error.data,
                    channel: None,
                    retry: Retry::Final,
                }
                .into_error(&record.attempts))
            }
        }
    }

    /// Build the onion for `path`, send it and wait for the result.
    async fn send(
        &self,
        path: &Path,
        part: Part,
        height: u32,
        may_retry_height: bool,
    ) -> Result<Outcome, PluginError> {
        let (state, invoice, node) = (self.state, self.invoice, self.node);
        // Core Lightning computes CLTVs relative to the next block.
        let onion = build_onion_hops(
            path,
            self.amount_msat,
            &RecipientFields {
                payment_secret: invoice.payment_secret,
                payment_metadata: invoice.payment_metadata.clone(),
            },
            height + 1,
        )
        .map_err(|err| error!("cannot build the onion: {err}"))?;
        let created = cln::createonion(state, &onion_json(&onion), &invoice.payment_hash).await?;

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
            // lightningd adds `delay` to its own base height, the higher of
            // its tip and its cached header count, which may lag behind the
            // header count `height` came from. Counting from the tip (the
            // lowest base it can use) never gives the first hop less than
            // the onion expects; a few blocks more are accepted.
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
                amount_msat: self.send_msat,
                total_amount_msat: self.amount_msat,
                part,
                destination: invoice.destination.as_deref(),
                invstring: Some(&self.request.invoice),
                label: self.request.label.as_deref(),
                description: self.request.description.as_deref(),
            },
        )
        .await;
        if let Err(err) = sent {
            if err.code != PAY_TRY_OTHER_ROUTE {
                return Err(PluginError::new(err.code, &err.message, err.data));
            }
            // Our own channel refused the HTLC right away: try another one.
            log::warn!("sendonion failed locally: {}", err.message);
            return Ok(Outcome::Failed(Failure {
                code: err.code,
                message: err.message,
                data: err.data,
                channel: Some(first.short_channel_id),
                retry: Retry::Exclude(first.short_channel_id),
            }));
        }

        match cln::waitsendpay(state, &invoice.payment_hash, part).await? {
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
                Ok(Outcome::Failed(
                    self.understand_failure(path, failure, error, height, may_retry_height)
                        .await,
                ))
            }
        }
    }

    /// Work out where the payment failed and what the next attempt should
    /// do: learn a carried `channel_update`, update the scorer, and decide
    /// whether to avoid the channel, retry it or stop.
    async fn understand_failure(
        &self,
        path: &Path,
        failure: cln::PaymentFailure,
        error: clightningrpc_common::errors::RpcError,
        height: u32,
        may_retry_height: bool,
    ) -> Failure {
        let hash = &self.invoice.payment_hash;
        let raw = failure.raw_message.as_deref().and_then(|raw| {
            hex::decode(raw)
                .map_err(|err| log::warn!("payment {hash}: bad raw_message {raw}: {err}"))
                .ok()
        });
        let parsed = raw.as_deref().and_then(|raw| {
            parse_failure_message(raw)
                .map_err(|err| log::warn!("payment {hash}: cannot decode the failure: {err}"))
                .ok()
        });
        let failcode = failure
            .failcode
            .and_then(|code| u16::try_from(code).ok())
            .or(parsed.as_ref().map(|f| f.failcode));
        let update = parsed.and_then(|f| f.channel_update);
        let kind = classify_failure(failcode, update.as_ref());

        let point = failure
            .erring_index
            .and_then(|index| locate_failure(path, index));
        let mut policy_learned = false;
        if let Some(point) = &point {
            log::info!("payment {hash} failed at {point:?}: {kind:?}");
            if let Some(update) = &update {
                policy_learned =
                    learn_channel_update(self.state, self.ctx, hash, point, update).await;
            }
            if kind.penalise_liquidity() {
                if let Some(scid) = point.penalised_channel() {
                    self.ctx.payment_failed(path, scid);
                }
            }
        }
        let channel = point.as_ref().and_then(|p| p.channel);
        let at_destination = point
            .as_ref()
            .is_none_or(|p| p.at_recipient || p.in_blinded_path);
        let retry = match channel {
            _ if at_destination => match raw.as_deref().and_then(reported_height) {
                Some(reported) if may_retry_height && reported > height => Retry::Height(reported),
                _ => Retry::Final,
            },
            Some(channel) if kind == FailureKind::Policy && policy_learned => {
                Retry::Policy(channel)
            }
            Some(channel) => Retry::Exclude(channel),
            None => Retry::Final,
        };
        let (mut code, data) = enrich_failure(error.code, error.data, point.as_ref());
        let mut message = error.message;
        if retry == Retry::Final && failcode == Some(INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS) {
            code = PAY_DESTINATION_PERM_FAIL;
            message = format!(
                "Destination said it doesn't know invoice: incorrect_or_unknown_payment_details ({message})"
            );
        }
        Failure {
            code,
            message,
            data,
            channel,
            retry,
        }
    }
}

fn onion_json(onion: &barq_common::onion::OnionHops) -> Vec<OnionHopJson> {
    onion
        .hops
        .iter()
        .map(|hop| OnionHopJson {
            pubkey: hop.pubkey.to_string(),
            payload: hex::encode(&hop.payload),
        })
        .collect()
}

/// The blinded path to use when every path of a BOLT12 invoice starts at
/// our own node, which LDK cannot route from.
fn introduced_by_us<'a>(
    invoice: &'a DecodedInvoice,
    node: &NodeContext,
) -> Option<(&'a BlindedPayInfo, &'a BlindedPath)> {
    let Payee::Blinded { paths, .. } = &invoice.payee else {
        return None;
    };
    let ours = |path: &BlindedPath| match &path.introduction_node {
        IntroductionNode::NodeId(id) => *id == node.our_id,
        // `NodeOne` is the channel end with the lesser node id.
        IntroductionNode::DirectedShortChannelId(direction, scid) => {
            node.first_hops.iter().any(|hop| {
                hop.scid == *scid
                    && matches!(direction, Direction::NodeOne)
                        == (node.our_id.serialize() < hop.peer_id.serialize())
            })
        }
    };
    if paths.is_empty() || !paths.iter().all(|(_, path)| ours(path)) {
        return None;
    }
    paths.first().map(|(payinfo, path)| (payinfo, path))
}

/// Learn the policy a forwarding node attached to its failure: apply the
/// `channel_update` to the LDK graph right away and hand it to gossipd,
/// which validates it and keeps it in the `gossip_store`. Returns whether
/// the graph took it.
///
/// Only an update for the channel the failure was reported on is accepted.
async fn learn_channel_update(
    state: &State,
    ctx: &NetworkContext,
    payment_hash: &str,
    point: &FailurePoint,
    carried: &CarriedUpdate,
) -> bool {
    let contents = carried.contents();
    let channel = scid::to_cln(contents.short_channel_id);
    if point.channel != Some(contents.short_channel_id) {
        log::warn!(
            "payment {payment_hash}: ignoring a channel_update for {channel} returned by a \
             failure on {:?}",
            point.channel.map(scid::to_cln)
        );
        return false;
    }
    let applied = match ctx.apply_channel_update(&carried.update) {
        Ok(()) => {
            log::info!(
                "payment {payment_hash}: applied channel_update for {channel} (fee_base_msat {}, \
                 fee_proportional_millionths {}, cltv_expiry_delta {}, htlc_minimum_msat {}, \
                 htlc_maximum_msat {}, disabled {})",
                contents.fee_base_msat,
                contents.fee_proportional_millionths,
                contents.cltv_expiry_delta,
                contents.htlc_minimum_msat,
                contents.htlc_maximum_msat,
                carried.disables_channel()
            );
            true
        }
        Err(err) => {
            log::debug!(
                "payment {payment_hash}: LDK graph rejected the channel_update for {channel}: {err}"
            );
            false
        }
    };
    if let Err(err) = cln::addgossip(state, &carried.raw).await {
        log::debug!(
            "payment {payment_hash}: gossipd rejected the channel_update for {channel}: {err:?}"
        );
    }
    applied
}

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
