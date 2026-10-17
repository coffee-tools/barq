//! Decode BOLT11 and BOLT12 invoices through Core Lightning `decode` and
//! turn them into what the router needs.
use std::str::FromStr;

use serde::Deserialize;
use serde_json::json;

use clightningrpc_plugin::error;
use clightningrpc_plugin::errors::PluginError;

use barq_common::ldk::blinded_path::{BlindedHop, BlindedPath, Direction, IntroductionNode};
use barq_common::ldk::ln::features::{
    BlindedHopFeatures, Bolt11InvoiceFeatures, Bolt12InvoiceFeatures,
};
use barq_common::ldk::offers::invoice::BlindedPayInfo;
use barq_common::ldk::routing::gossip::RoutingFees;
use barq_common::ldk::routing::router::{RouteHint, RouteHintHop};
use barq_common::secp256k1::PublicKey;
use barq_common::strategy::Payee;
use barq_common::{scid, Network};

use crate::plugin::State;

/// Raw `decode` response; only the fields we care about.
///
/// See: https://docs.corelightning.org/reference/lightning-decode
#[derive(Debug, Deserialize)]
struct DecodeResponse {
    #[serde(rename = "type")]
    kind: String,
    valid: bool,
    // BOLT11
    currency: Option<String>,
    payee: Option<String>,
    amount_msat: Option<u64>,
    payment_hash: Option<String>,
    min_final_cltv_expiry: Option<u32>,
    payment_secret: Option<String>,
    payment_metadata: Option<String>,
    created_at: Option<u64>,
    expiry: Option<u64>,
    features: Option<String>,
    description_hash: Option<String>,
    #[serde(default)]
    routes: Vec<Vec<RouteHintHopJson>>,
    // BOLT12
    invoice_node_id: Option<String>,
    invoice_amount_msat: Option<u64>,
    invoice_payment_hash: Option<String>,
    invoice_features: Option<String>,
    invoice_created_at: Option<u64>,
    invoice_relative_expiry: Option<u64>,
    #[serde(default)]
    invoice_paths: Vec<BlindedPathJson>,
}

#[derive(Debug, Deserialize)]
struct RouteHintHopJson {
    pubkey: String,
    short_channel_id: String,
    fee_base_msat: u64,
    fee_proportional_millionths: u32,
    cltv_expiry_delta: u32,
}

#[derive(Debug, Deserialize)]
struct BlindedPathJson {
    first_node_id: Option<String>,
    first_scid: Option<String>,
    first_scid_dir: Option<u8>,
    /// Renamed from `blinding` to `first_path_key` in newer releases.
    #[serde(alias = "blinding")]
    first_path_key: String,
    payinfo: PayInfoJson,
    path: Vec<BlindedHopJson>,
}

#[derive(Debug, Deserialize)]
struct PayInfoJson {
    fee_base_msat: u64,
    fee_proportional_millionths: u32,
    cltv_expiry_delta: u32,
    #[serde(default)]
    htlc_minimum_msat: Option<u64>,
    #[serde(default)]
    htlc_maximum_msat: Option<u64>,
    features: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BlindedHopJson {
    blinded_node_id: String,
    encrypted_recipient_data: String,
}

/// The kind of invoice we are paying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvoiceKind {
    Bolt11,
    Bolt12,
}

/// Everything the payment flow needs from an invoice.
#[derive(Debug, Clone)]
pub struct DecodedInvoice {
    pub kind: InvoiceKind,
    pub payee: Payee,
    /// The payee node id when known (BOLT11 payee, BOLT12 `invoice_node_id`).
    pub destination: Option<String>,
    pub payment_hash: String,
    pub amount_msat: Option<u64>,
    pub final_cltv_delta: u32,
    pub payment_secret: Option<[u8; 32]>,
    pub payment_metadata: Option<Vec<u8>>,
    /// Unix timestamp after which the invoice expires.
    pub expiry_time: Option<u64>,
    /// Network the invoice was issued for, when encoded in it.
    pub network: Option<Network>,
    /// BOLT11 `h`: the invoice commits to a description it does not carry.
    pub description_hash: Option<String>,
}

fn pubkey(s: &str, what: &str) -> Result<PublicKey, PluginError> {
    PublicKey::from_str(s).map_err(|_| error!("Invalid {what} `{s}`"))
}

fn hex32(s: &str, what: &str) -> Result<[u8; 32], PluginError> {
    let bytes = hex::decode(s).map_err(|_| error!("Invalid {what} `{s}`"))?;
    bytes
        .try_into()
        .map_err(|_| error!("Invalid {what} `{s}`: expected 32 bytes"))
}

/// Decode `invstring` with Core Lightning.
pub async fn decode(state: &State, invstring: &str) -> Result<DecodedInvoice, PluginError> {
    let decoded: DecodeResponse = state
        .call("decode", json!({ "string": invstring }))
        .await
        .map_err(|err| PluginError::new(err.code, &err.message, err.data))?;
    if !decoded.valid {
        return Err(error!(
            "Invalid invoice: `{}` failed to decode",
            decoded.kind
        ));
    }
    match decoded.kind.as_str() {
        "bolt11 invoice" => bolt11(decoded),
        "bolt12 invoice" => bolt12(decoded),
        other => Err(error!(
            "Unsupported invoice type `{other}`: barq pays bolt11 and bolt12 invoices"
        )),
    }
}

fn bolt11(decoded: DecodeResponse) -> Result<DecodedInvoice, PluginError> {
    let payee = decoded
        .payee
        .ok_or_else(|| error!("bolt11 invoice without payee"))?;
    let node_id = pubkey(&payee, "payee")?;
    let payment_hash = decoded
        .payment_hash
        .ok_or_else(|| error!("bolt11 invoice without payment_hash"))?;

    // See: https://github.com/lightning/bolts/blob/master/11-payment-encoding.md#human-readable-part
    let network = match decoded.currency.as_deref() {
        Some("bc") => Some(Network::Bitcoin),
        Some("tb") => Some(Network::Testnet),
        Some("tbs") => Some(Network::Signet),
        Some("bcrt") => Some(Network::Regtest),
        Some(other) => return Err(error!("Unknown currency: {other}")),
        None => None,
    };

    let mut route_hints = Vec::new();
    for route in decoded.routes {
        let mut hops = Vec::new();
        for hop in route {
            hops.push(RouteHintHop {
                src_node_id: pubkey(&hop.pubkey, "route hint node")?,
                short_channel_id: scid::from_cln(&hop.short_channel_id)
                    .map_err(|e| error!("{e}"))?,
                fees: RoutingFees {
                    base_msat: hop.fee_base_msat as u32,
                    proportional_millionths: hop.fee_proportional_millionths,
                },
                cltv_expiry_delta: hop.cltv_expiry_delta as u16,
                htlc_minimum_msat: None,
                htlc_maximum_msat: None,
            });
        }
        if !hops.is_empty() {
            route_hints.push(RouteHint(hops));
        }
    }

    let features = decoded
        .features
        .as_deref()
        .map(hex::decode)
        .transpose()
        .map_err(|_| error!("Invalid invoice features"))?
        .map(Bolt11InvoiceFeatures::from_be_bytes);

    let payment_secret = decoded
        .payment_secret
        .as_deref()
        .map(|s| hex32(s, "payment_secret"))
        .transpose()?;
    let payment_metadata = decoded
        .payment_metadata
        .as_deref()
        .map(hex::decode)
        .transpose()
        .map_err(|_| error!("Invalid payment_metadata"))?;

    Ok(DecodedInvoice {
        kind: InvoiceKind::Bolt11,
        payee: Payee::Clear {
            node_id,
            route_hints,
            features,
        },
        destination: Some(payee),
        payment_hash,
        amount_msat: decoded.amount_msat,
        final_cltv_delta: decoded.min_final_cltv_expiry.unwrap_or(18),
        payment_secret,
        payment_metadata,
        description_hash: decoded.description_hash.clone(),
        expiry_time: match (decoded.created_at, decoded.expiry) {
            (Some(created), Some(expiry)) => Some(created + expiry),
            _ => None,
        },
        network,
    })
}

fn bolt12(decoded: DecodeResponse) -> Result<DecodedInvoice, PluginError> {
    let payment_hash = decoded
        .invoice_payment_hash
        .ok_or_else(|| error!("bolt12 invoice without invoice_payment_hash"))?;
    let amount_msat = decoded
        .invoice_amount_msat
        .ok_or_else(|| error!("bolt12 invoice without invoice_amount_msat"))?;
    if decoded.invoice_paths.is_empty() {
        return Err(error!("bolt12 invoice without invoice_paths"));
    }

    let mut paths = Vec::with_capacity(decoded.invoice_paths.len());
    for path in decoded.invoice_paths {
        let introduction_node = match (&path.first_node_id, &path.first_scid) {
            (Some(node), _) => IntroductionNode::NodeId(pubkey(node, "first_node_id")?),
            (None, Some(first_scid)) => {
                let direction = match path.first_scid_dir {
                    Some(0) => Direction::NodeOne,
                    Some(1) => Direction::NodeTwo,
                    _ => return Err(error!("bolt12 blinded path without first_scid_dir")),
                };
                IntroductionNode::DirectedShortChannelId(
                    direction,
                    scid::from_cln(first_scid).map_err(|e| error!("{e}"))?,
                )
            }
            (None, None) => return Err(error!("bolt12 blinded path without introduction node")),
        };
        let blinding_point = pubkey(&path.first_path_key, "first_path_key")?;
        let mut blinded_hops = Vec::with_capacity(path.path.len());
        for hop in path.path {
            blinded_hops.push(BlindedHop {
                blinded_node_id: pubkey(&hop.blinded_node_id, "blinded_node_id")?,
                encrypted_payload: hex::decode(&hop.encrypted_recipient_data)
                    .map_err(|_| error!("Invalid encrypted_recipient_data"))?,
            });
        }
        if blinded_hops.is_empty() {
            return Err(error!("bolt12 blinded path without hops"));
        }
        let features = path
            .payinfo
            .features
            .as_deref()
            .map(hex::decode)
            .transpose()
            .map_err(|_| error!("Invalid blinded path features"))?
            .map(BlindedHopFeatures::from_be_bytes)
            .unwrap_or_else(BlindedHopFeatures::empty);
        let payinfo = BlindedPayInfo {
            fee_base_msat: path.payinfo.fee_base_msat as u32,
            fee_proportional_millionths: path.payinfo.fee_proportional_millionths,
            cltv_expiry_delta: path.payinfo.cltv_expiry_delta as u16,
            htlc_minimum_msat: path.payinfo.htlc_minimum_msat.unwrap_or(0),
            htlc_maximum_msat: path.payinfo.htlc_maximum_msat.unwrap_or(u64::MAX),
            features,
        };
        paths.push((
            payinfo,
            BlindedPath {
                introduction_node,
                blinding_point,
                blinded_hops,
            },
        ));
    }

    let features = decoded
        .invoice_features
        .as_deref()
        .map(hex::decode)
        .transpose()
        .map_err(|_| error!("Invalid invoice features"))?
        .map(Bolt12InvoiceFeatures::from_be_bytes);

    Ok(DecodedInvoice {
        kind: InvoiceKind::Bolt12,
        payee: Payee::Blinded { paths, features },
        destination: decoded.invoice_node_id,
        payment_hash,
        amount_msat: Some(amount_msat),
        final_cltv_delta: 0,
        payment_secret: None,
        payment_metadata: None,
        description_hash: None,
        expiry_time: match (decoded.invoice_created_at, decoded.invoice_relative_expiry) {
            (Some(created), Some(expiry)) => Some(created + expiry),
            (Some(created), None) => Some(created + 7200),
            _ => None,
        },
        network: None,
    })
}
