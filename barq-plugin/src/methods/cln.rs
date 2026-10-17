//! Typed wrappers around the Core Lightning RPC calls barq relies on.
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use clightningrpc_plugin::errors::PluginError;

use barq_common::scid;
use barq_common::secp256k1::PublicKey;
use barq_common::strategy::FirstHop;

use crate::plugin::State;

fn rpc_err(err: clightningrpc_common::errors::RpcError) -> PluginError {
    PluginError::new(err.code, &err.message, err.data)
}

/// Response from `getinfo`.
///
/// See: https://docs.corelightning.org/reference/lightning-getinfo
#[derive(Debug, Deserialize)]
pub struct NodeInfo {
    pub id: String,
    pub network: String,
    pub blockheight: u32,
}

pub async fn getinfo(state: &State) -> Result<NodeInfo, PluginError> {
    state.call("getinfo", json!({})).await.map_err(rpc_err)
}

#[derive(Debug, Deserialize)]
struct ListPeerChannels {
    channels: Vec<PeerChannel>,
}

#[derive(Debug, Deserialize)]
struct ChannelAlias {
    local: Option<String>,
    remote: Option<String>,
}

/// The subset of `listpeerchannels` we need to describe a first hop.
#[derive(Debug, Deserialize)]
struct PeerChannel {
    peer_id: String,
    #[serde(default)]
    peer_connected: bool,
    state: String,
    short_channel_id: Option<String>,
    alias: Option<ChannelAlias>,
    total_msat: Option<u64>,
    spendable_msat: Option<u64>,
    minimum_htlc_out_msat: Option<u64>,
    maximum_htlc_out_msat: Option<u64>,
    #[serde(default)]
    private: bool,
}

/// Our usable channels, as LDK first hops.
pub async fn first_hops(state: &State) -> Result<Vec<FirstHop>, PluginError> {
    let response: ListPeerChannels = state
        .call("listpeerchannels", json!({}))
        .await
        .map_err(rpc_err)?;
    let mut hops = Vec::new();
    for channel in response.channels {
        if channel.state != "CHANNELD_NORMAL" || !channel.peer_connected {
            continue;
        }
        let Ok(peer_id) = PublicKey::from_str(&channel.peer_id) else {
            continue;
        };
        // LDK must address the channel the way the peer expects to see it
        // in a forwarded onion, while Core Lightning must recognise the
        // identifier we pass back in `sendonion`.
        let (ldk_scid, cln_channel) = match (&channel.short_channel_id, &channel.alias) {
            (Some(real), _) => (real.clone(), real.clone()),
            (None, Some(alias)) => match (&alias.remote, &alias.local) {
                (Some(remote), Some(local)) => (remote.clone(), local.clone()),
                (Some(remote), None) => (remote.clone(), remote.clone()),
                _ => continue,
            },
            _ => continue,
        };
        let Ok(ldk_scid) = scid::from_cln(&ldk_scid) else {
            log::warn!("skipping channel with unparsable scid `{ldk_scid}`");
            continue;
        };
        let spendable = channel.spendable_msat.unwrap_or(0);
        hops.push(FirstHop {
            peer_id,
            scid: ldk_scid,
            cln_channel,
            capacity_sat: channel.total_msat.unwrap_or(0) / 1000,
            spendable_msat: spendable,
            htlc_minimum_msat: channel.minimum_htlc_out_msat.unwrap_or(0),
            htlc_maximum_msat: channel.maximum_htlc_out_msat.unwrap_or(spendable),
            is_public: !channel.private,
        });
    }
    Ok(hops)
}

/// One hop for `createonion`.
#[derive(Debug, Serialize)]
pub struct OnionHopJson {
    pub pubkey: String,
    pub payload: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateOnionResponse {
    pub onion: String,
    pub shared_secrets: Vec<String>,
}

pub async fn createonion(
    state: &State,
    hops: &[OnionHopJson],
    payment_hash: &str,
) -> Result<CreateOnionResponse, PluginError> {
    state
        .call(
            "createonion",
            json!({
                "hops": hops,
                "assocdata": payment_hash,
            }),
        )
        .await
        .map_err(rpc_err)
}

/// The first hop of a `sendonion` call.
#[derive(Debug, Serialize)]
pub struct SendOnionFirstHop {
    pub id: String,
    pub channel: String,
    pub amount_msat: u64,
    pub delay: u32,
}

/// What `sendonion` and `waitsendpay` return.
///
/// See: https://docs.corelightning.org/reference/lightning-sendonion
#[derive(Debug, Deserialize, Serialize)]
pub struct SendPayStatus {
    pub id: u64,
    pub payment_hash: String,
    pub status: String,
    #[serde(default)]
    pub groupid: Option<u64>,
    #[serde(default)]
    pub partid: Option<u64>,
    #[serde(default)]
    pub amount_msat: Option<u64>,
    #[serde(default)]
    pub amount_sent_msat: Option<u64>,
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default)]
    pub created_at: Option<u64>,
    #[serde(default)]
    pub completed_at: Option<f64>,
    #[serde(default)]
    pub payment_preimage: Option<String>,
}

pub struct SendOnionRequest<'a> {
    pub onion: &'a str,
    pub first_hop: SendOnionFirstHop,
    pub payment_hash: &'a str,
    pub shared_secrets: &'a [String],
    /// What this part delivers to the destination.
    pub amount_msat: u64,
    /// What the whole payment delivers (more than `amount_msat` for a
    /// partial payment).
    pub total_amount_msat: u64,
    pub part: Part,
    pub destination: Option<&'a str>,
    pub invstring: Option<&'a str>,
    pub label: Option<&'a str>,
    /// The description a `description_hash` invoice commits to, shown by
    /// `listsendpays` and `listpays`.
    pub description: Option<&'a str>,
}

/// Where an attempt sits in `listsendpays`: like `pay`, every attempt of
/// one payment shares a `groupid` and gets the next `partid`, so
/// `listpays` shows one payment and `delpay` can address each attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    pub groupid: u64,
    pub partid: u64,
}

/// `sendonion`, keeping Core Lightning's error code so the caller can tell
/// a local channel failure (204) from other errors.
pub async fn sendonion_raw(
    state: &State,
    req: SendOnionRequest<'_>,
) -> Result<SendPayStatus, clightningrpc_common::errors::RpcError> {
    let mut payload = json!({
        "onion": req.onion,
        "first_hop": req.first_hop,
        "payment_hash": req.payment_hash,
        "shared_secrets": req.shared_secrets,
        "amount_msat": req.amount_msat,
        "total_amount_msat": req.total_amount_msat,
        "partid": req.part.partid,
        "groupid": req.part.groupid,
    });
    if let Some(destination) = req.destination {
        payload["destination"] = Value::String(destination.to_owned());
    }
    if let Some(invstring) = req.invstring {
        payload["bolt11"] = Value::String(invstring.to_owned());
    }
    if let Some(label) = req.label {
        payload["label"] = Value::String(label.to_owned());
    }
    if let Some(description) = req.description {
        payload["description"] = Value::String(description.to_owned());
    }
    state.call("sendonion", payload).await
}

/// Error details Core Lightning attaches to a failed `waitsendpay`.
///
/// See: https://docs.corelightning.org/reference/lightning-waitsendpay
#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)] // kept for logging and future retry logic
pub struct PaymentFailure {
    #[serde(default)]
    pub erring_index: Option<u32>,
    #[serde(default)]
    pub erring_node: Option<String>,
    #[serde(default)]
    pub erring_channel: Option<String>,
    #[serde(default)]
    pub erring_direction: Option<u32>,
    #[serde(default)]
    pub failcode: Option<u32>,
    #[serde(default)]
    pub failcodename: Option<String>,
    /// The decrypted BOLT4 failure message (hex): the failure code
    /// followed by its fields, see `barq_common::failure`.
    #[serde(default)]
    pub raw_message: Option<String>,
}

/// Hand a BOLT7 gossip message (with its type prefix) to gossipd, which
/// validates it and appends it to the `gossip_store`.
///
/// See: https://docs.corelightning.org/reference/lightning-addgossip
pub async fn addgossip(state: &State, message: &[u8]) -> Result<(), PluginError> {
    let _: Value = state
        .call("addgossip", json!({ "message": hex::encode(message) }))
        .await
        .map_err(rpc_err)?;
    Ok(())
}

/// Outcome of `waitsendpay`.
pub enum WaitSendPay {
    Complete(SendPayStatus),
    /// The payment failed; the original error is kept so the caller can
    /// return it unchanged to the user.
    Failed {
        failure: PaymentFailure,
        error: clightningrpc_common::errors::RpcError,
    },
}

pub async fn waitsendpay(
    state: &State,
    payment_hash: &str,
    part: Part,
) -> Result<WaitSendPay, PluginError> {
    let payload = json!({
        "payment_hash": payment_hash,
        "partid": part.partid,
        "groupid": part.groupid,
    });
    wait_result(state.call::<_, SendPayStatus>("waitsendpay", payload).await)
}

fn wait_result(
    result: Result<SendPayStatus, clightningrpc_common::errors::RpcError>,
) -> Result<WaitSendPay, PluginError> {
    match result {
        Ok(status) => Ok(WaitSendPay::Complete(status)),
        // 203: permanent failure, 204: failure along the route (retryable).
        Err(error) if error.code == 203 || error.code == 204 => {
            let failure = error
                .data
                .clone()
                .and_then(|data| serde_json::from_value(data).ok())
                .unwrap_or_default();
            Ok(WaitSendPay::Failed { failure, error })
        }
        Err(error) => Err(rpc_err(error)),
    }
}

/// The earlier attempts to pay `payment_hash`, oldest first.
///
/// See: https://docs.corelightning.org/reference/lightning-listsendpays
pub async fn listsendpays(
    state: &State,
    payment_hash: &str,
) -> Result<Vec<SendPayStatus>, PluginError> {
    #[derive(Debug, Deserialize)]
    struct ListSendPays {
        payments: Vec<SendPayStatus>,
    }
    let list: ListSendPays = state
        .call("listsendpays", json!({ "payment_hash": payment_hash }))
        .await
        .map_err(rpc_err)?;
    Ok(list.payments)
}

/// The best known chain height: the block headers the backend has seen,
/// which may be ahead of the blocks lightningd processed. Like xpay, CLTVs
/// are computed from it so a node still catching up can pay.
///
/// See: https://docs.corelightning.org/reference/lightning-getchaininfo
pub async fn headercount(state: &State) -> Option<u32> {
    let info: Value = state
        .call("getchaininfo", json!({ "last_height": 0 }))
        .await
        .map_err(|err| log::debug!("getchaininfo failed: {}", err.message))
        .ok()?;
    info["headercount"].as_u64().map(|h| h as u32)
}

/// A payment to ourselves: `sendpay` with an empty route, as `pay` does.
pub struct SelfPayRequest<'a> {
    pub payment_hash: &'a str,
    pub amount_msat: u64,
    pub payment_secret: Option<[u8; 32]>,
    pub payment_metadata: Option<&'a [u8]>,
    pub invstring: &'a str,
    pub label: Option<&'a str>,
    pub description: Option<&'a str>,
    pub part: Part,
}

pub async fn selfpay(state: &State, req: SelfPayRequest<'_>) -> Result<WaitSendPay, PluginError> {
    let mut payload = json!({
        "route": [],
        "payment_hash": req.payment_hash,
        "amount_msat": req.amount_msat,
        "bolt11": req.invstring,
        "groupid": req.part.groupid,
    });
    if let Some(secret) = req.payment_secret {
        payload["payment_secret"] = Value::String(hex::encode(secret));
    }
    if let Some(metadata) = req.payment_metadata {
        payload["payment_metadata"] = Value::String(hex::encode(metadata));
    }
    if let Some(label) = req.label {
        payload["label"] = Value::String(label.to_owned());
    }
    if let Some(description) = req.description {
        payload["description"] = Value::String(description.to_owned());
    }
    // A self-payment completes (or fails) right away.
    match state.call::<_, SendPayStatus>("sendpay", payload).await {
        Ok(status) if status.status == "complete" => Ok(WaitSendPay::Complete(status)),
        Ok(_) => {
            waitsendpay(
                state,
                req.payment_hash,
                Part {
                    partid: 0,
                    ..req.part
                },
            )
            .await
        }
        Err(error) => wait_result(Err(error)),
    }
}

/// An onion whose first hop is our own node: lightningd unwraps it as if a
/// peer had sent it. Used when we are the introduction node of the
/// recipient's blinded path, which `sendonion` cannot start from.
pub struct InjectRequest<'a> {
    pub onion: &'a str,
    pub payment_hash: &'a str,
    pub amount_msat: u64,
    pub cltv_expiry: u32,
    pub destination_msat: u64,
    pub part: Part,
    pub invstring: &'a str,
    pub label: Option<&'a str>,
    pub destination: Option<&'a str>,
}

/// `injectpaymentonion` blocks until the payment completes. On failure
/// Core Lightning returns the still encrypted onion reply, so the failure
/// is reported as a final one without its details.
///
/// See: https://docs.corelightning.org/reference/lightning-injectpaymentonion
pub async fn injectpaymentonion(
    state: &State,
    req: InjectRequest<'_>,
) -> Result<WaitSendPay, PluginError> {
    let mut payload = json!({
        "onion": req.onion,
        "payment_hash": req.payment_hash,
        "amount_msat": req.amount_msat,
        "cltv_expiry": req.cltv_expiry,
        "partid": req.part.partid,
        "groupid": req.part.groupid,
        "destination_msat": req.destination_msat,
        "invstring": req.invstring,
    });
    if let Some(label) = req.label {
        payload["label"] = Value::String(label.to_owned());
    }
    if let Some(destination) = req.destination {
        payload["destination"] = Value::String(destination.to_owned());
    }
    #[derive(Debug, Deserialize)]
    struct Injected {
        payment_preimage: String,
        created_at: Option<u64>,
        completed_at: Option<f64>,
    }
    match state
        .call::<_, Injected>("injectpaymentonion", payload)
        .await
    {
        Ok(done) => Ok(WaitSendPay::Complete(SendPayStatus {
            id: 0,
            payment_hash: req.payment_hash.to_owned(),
            status: "complete".to_owned(),
            groupid: Some(req.part.groupid),
            partid: Some(req.part.partid),
            amount_msat: Some(req.destination_msat),
            amount_sent_msat: Some(req.amount_msat),
            destination: req.destination.map(str::to_owned),
            created_at: done.created_at,
            completed_at: done.completed_at,
            payment_preimage: Some(done.payment_preimage),
        })),
        // 218: the payment failed; the reason is inside the onion reply.
        Err(error) if error.code == 218 => Ok(WaitSendPay::Failed {
            failure: PaymentFailure::default(),
            error: clightningrpc_common::errors::RpcError {
                code: 203,
                message: format!(
                    "{} (as introduction node of the blinded path)",
                    error.message
                ),
                data: error.data,
            },
        }),
        Err(error) => Err(rpc_err(error)),
    }
}

/// Whether `cln-xpay` currently takes over `pay` (`xpay-handle-pay`).
/// `false` when xpay is not running.
pub async fn xpay_handles_pay(state: &State) -> bool {
    match state
        .call::<_, Value>("listconfigs", json!({ "config": "xpay-handle-pay" }))
        .await
    {
        Ok(configs) => configs["configs"]["xpay-handle-pay"]["value_bool"]
            .as_bool()
            .unwrap_or(false),
        Err(_) => false,
    }
}
