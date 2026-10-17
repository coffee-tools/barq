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
    pub amount_msat: u64,
    pub destination: Option<&'a str>,
    pub invstring: Option<&'a str>,
    pub label: Option<&'a str>,
}

pub async fn sendonion(
    state: &State,
    req: SendOnionRequest<'_>,
) -> Result<SendPayStatus, PluginError> {
    let mut payload = json!({
        "onion": req.onion,
        "first_hop": req.first_hop,
        "payment_hash": req.payment_hash,
        "shared_secrets": req.shared_secrets,
        "amount_msat": req.amount_msat,
        "partid": 0,
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
    state.call("sendonion", payload).await.map_err(rpc_err)
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
    groupid: Option<u64>,
) -> Result<WaitSendPay, PluginError> {
    let mut payload = json!({
        "payment_hash": payment_hash,
        "partid": 0,
    });
    if let Some(groupid) = groupid {
        payload["groupid"] = json!(groupid);
    }
    match state.call::<_, SendPayStatus>("waitsendpay", payload).await {
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
