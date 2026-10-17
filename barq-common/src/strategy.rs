//! Routing strategy interface.
//!
//! A strategy receives everything it needs to know about the payment
//! (payer, payee, amount, our own channels) and returns an LDK [`Path`]
//! that the plugin turns into an onion and hands to Core Lightning.
use std::str::FromStr;

use anyhow::Result;

use lampo_common::ldk::blinded_path::BlindedPath;
use lampo_common::ldk::ln::features::{Bolt11InvoiceFeatures, Bolt12InvoiceFeatures};
use lampo_common::ldk::offers::invoice::BlindedPayInfo;
use lampo_common::ldk::routing::router::{Path, RouteHint};
use lampo_common::secp256k1::PublicKey;

#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
pub enum StrategyKind {
    /// Pay a peer we have a direct channel with, without any path finding.
    Direct,
    /// LDK path finding with the probabilistic scorer.
    #[default]
    Probabilistic,
}

impl StrategyKind {
    /// Every built-in strategy, in tie-break order.
    pub const ALL: [StrategyKind; 2] = [StrategyKind::Direct, StrategyKind::Probabilistic];

    pub fn name(&self) -> &'static str {
        match self {
            StrategyKind::Direct => "direct",
            StrategyKind::Probabilistic => "probabilistic",
        }
    }
}

impl FromStr for StrategyKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_lowercase().as_str() {
            "direct" => Ok(Self::Direct),
            "probabilistic" | "ldk" => Ok(Self::Probabilistic),
            _ => anyhow::bail!("Strategy `{s}` not found"),
        }
    }
}

/// The `Strategy` trait defines an interface for routing strategies used within
/// Barq.
pub trait Strategy {
    /// Whether the strategy can be applied to the given input
    fn can_apply(&self, input: &RouteInput) -> Result<bool>;

    /// Route the payment using the strategy
    /// return error if execution unsuccessful
    fn route(&self, input: &RouteInput) -> Result<RouteOutput>;
}

/// One of our own channels, as reported by Core Lightning `listpeerchannels`.
///
/// These are handed to the router as "first hops" so that routing works
/// for unannounced channels too and uses our real spendable balance.
#[derive(Debug, Clone)]
pub struct FirstHop {
    pub peer_id: PublicKey,
    /// The short channel id LDK should use for this channel: the real
    /// one when the channel is confirmed, otherwise the alias the peer
    /// gave us.
    pub scid: u64,
    /// The identifier Core Lightning accepts for this channel in
    /// `sendonion`'s `first_hop.channel`: the real scid or our local alias.
    pub cln_channel: String,
    pub capacity_sat: u64,
    pub spendable_msat: u64,
    pub htlc_minimum_msat: u64,
    pub htlc_maximum_msat: u64,
    pub is_public: bool,
}

/// Who we are paying.
#[derive(Debug, Clone)]
pub enum Payee {
    /// A BOLT11 invoice: the payee node id is known.
    Clear {
        node_id: PublicKey,
        route_hints: Vec<RouteHint>,
        features: Option<Bolt11InvoiceFeatures>,
    },
    /// A BOLT12 invoice: we only know blinded paths towards the payee.
    Blinded {
        paths: Vec<(BlindedPayInfo, BlindedPath)>,
        features: Option<Bolt12InvoiceFeatures>,
    },
}

impl Payee {
    /// The node id of the payee, when it is not hidden behind a blinded path.
    pub fn node_id(&self) -> Option<PublicKey> {
        match self {
            Payee::Clear { node_id, .. } => Some(*node_id),
            Payee::Blinded { .. } => None,
        }
    }
}

/// Represents input data required for routing a payment
#[derive(Debug, Clone)]
pub struct RouteInput {
    pub src_pubkey: PublicKey,
    pub payee: Payee,
    pub amount_msat: u64,
    /// The final CLTV delta requested by the payee (BOLT11
    /// `min_final_cltv_expiry`). Ignored for blinded payees, where the
    /// delta is part of the blinded path.
    pub final_cltv_delta: u32,
    /// Our own usable channels.
    pub first_hops: Vec<FirstHop>,
    /// Unix timestamp after which the invoice is no longer valid.
    pub expiry_time: Option<u64>,
    /// Force this strategy instead of letting barq rank every applicable
    /// one (issue #10).
    pub strategy: Option<StrategyKind>,
    /// Channels that failed in earlier attempts of this payment: routes
    /// must avoid them.
    pub excluded_channels: Vec<u64>,
}

/// Represents the output of a routing strategy: a single LDK path from us
/// to the payee.
#[derive(Debug, Clone)]
pub struct RouteOutput {
    pub path: Path,
}

impl RouteOutput {
    /// Total fee paid along the path, excluding the amount delivered.
    pub fn fee_msat(&self) -> u64 {
        self.path.fee_msat()
    }
}
