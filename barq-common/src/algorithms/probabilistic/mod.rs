//! LDK based routing: `find_route` over the shared network graph with the
//! probabilistic scorer.
use std::ops::Deref;
use std::sync::Arc;

use anyhow::Result;

use lampo_common::ldk::ln::channelmanager::{
    ChannelCounterparty, ChannelDetails, ChannelShutdownState,
};
use lampo_common::ldk::ln::features::InitFeatures;
use lampo_common::ldk::ln::types::ChannelId;
use lampo_common::ldk::routing::router::{find_route, PaymentParameters, RouteParameters};
use lampo_common::ldk::routing::scoring::ProbabilisticScoringFeeParameters;

use crate::network::NetworkContext;
use crate::strategy::{FirstHop, Payee, RouteInput, RouteOutput, Strategy};

/// A routing strategy that uses the LDK crates to find the best route.
pub struct LDKRoutingStrategy {
    ctx: Arc<NetworkContext>,
}

impl LDKRoutingStrategy {
    pub fn new(ctx: Arc<NetworkContext>) -> Self {
        Self { ctx }
    }

    fn payment_parameters(input: &RouteInput) -> Result<PaymentParameters> {
        let mut params = match &input.payee {
            Payee::Clear {
                node_id,
                route_hints,
                features,
            } => {
                let mut params = PaymentParameters::from_node_id(*node_id, input.final_cltv_delta);
                if let Some(features) = features {
                    params = params
                        .with_bolt11_features(features.clone())
                        .map_err(|_| anyhow::anyhow!("Invalid BOLT11 features"))?;
                }
                if !route_hints.is_empty() {
                    params = params
                        .with_route_hints(route_hints.clone())
                        .map_err(|_| anyhow::anyhow!("Invalid route hints"))?;
                }
                params
            }
            Payee::Blinded { paths, features } => {
                if paths.is_empty() {
                    anyhow::bail!("BOLT12 invoice without blinded paths");
                }
                let mut params = PaymentParameters::blinded(paths.clone());
                if let Some(features) = features {
                    params = params
                        .with_bolt12_features(features.clone())
                        .map_err(|_| anyhow::anyhow!("Invalid BOLT12 features"))?;
                }
                params
            }
        };
        if let Some(expiry) = input.expiry_time {
            params = params.with_expiry_time(expiry);
        }
        // Core Lightning `sendonion` sends one HTLC per call; keep the
        // route single-path until multi-part payments are wired in.
        params.max_path_count = 1;
        params
            .previously_failed_channels
            .extend(input.excluded_channels.iter().copied());
        Ok(params)
    }

    /// Present one of our channels to the router as an LDK `ChannelDetails`.
    fn channel_details(hop: &FirstHop) -> ChannelDetails {
        let mut id = [0u8; 32];
        id[..8].copy_from_slice(&hop.scid.to_be_bytes());
        ChannelDetails {
            channel_id: ChannelId::from_bytes(id),
            counterparty: ChannelCounterparty {
                node_id: hop.peer_id,
                features: InitFeatures::empty(),
                unspendable_punishment_reserve: 0,
                forwarding_info: None,
                outbound_htlc_minimum_msat: None,
                outbound_htlc_maximum_msat: None,
            },
            funding_txo: None,
            channel_type: None,
            short_channel_id: Some(hop.scid),
            outbound_scid_alias: None,
            inbound_scid_alias: None,
            channel_value_satoshis: hop.capacity_sat,
            unspendable_punishment_reserve: None,
            user_channel_id: 0,
            feerate_sat_per_1000_weight: None,
            balance_msat: hop.spendable_msat,
            outbound_capacity_msat: hop.spendable_msat,
            next_outbound_htlc_limit_msat: hop.spendable_msat.min(hop.htlc_maximum_msat),
            next_outbound_htlc_minimum_msat: hop.htlc_minimum_msat,
            inbound_capacity_msat: 0,
            confirmations_required: None,
            confirmations: None,
            force_close_spend_delay: None,
            is_outbound: true,
            is_channel_ready: true,
            channel_shutdown_state: Some(ChannelShutdownState::NotShuttingDown),
            is_usable: true,
            is_public: hop.is_public,
            inbound_htlc_minimum_msat: None,
            inbound_htlc_maximum_msat: None,
            config: None,
            pending_inbound_htlcs: vec![],
            pending_outbound_htlcs: vec![],
        }
    }
}

impl Strategy for LDKRoutingStrategy {
    /// LDK can always try, as long as we have at least one usable channel.
    fn can_apply(&self, input: &RouteInput) -> Result<bool> {
        Ok(!input.first_hops.is_empty())
    }

    fn route(&self, input: &RouteInput) -> Result<RouteOutput> {
        if input.first_hops.is_empty() {
            anyhow::bail!("No usable channel to route the payment from");
        }
        let payment_params = Self::payment_parameters(input)?;
        let route_params =
            RouteParameters::from_payment_params_and_value(payment_params, input.amount_msat);

        let first_hops: Vec<ChannelDetails> =
            input.first_hops.iter().map(Self::channel_details).collect();
        let first_hops_ref: Vec<&ChannelDetails> = first_hops.iter().collect();

        let fee_params = ProbabilisticScoringFeeParameters::default();
        let seed = self.ctx.random_seed();
        let (channels, nodes) = self.ctx.size();
        log::debug!(
            "ldk routing: {} first hops, graph has {channels} channels and {nodes} nodes",
            first_hops.len()
        );

        let route = {
            let scorer = self.ctx.scorer.lock().unwrap();
            find_route(
                &input.src_pubkey,
                &route_params,
                &self.ctx.graph,
                Some(&first_hops_ref),
                self.ctx.logger.deref(),
                &*scorer,
                &fee_params,
                &seed,
            )
            .map_err(|e| anyhow::anyhow!("Failed to find route: {}", e.err))?
        };

        let Some(mut path) = route.paths.into_iter().next() else {
            anyhow::bail!("LDK returned a route without paths");
        };
        one_hop_blinded_cltv(&mut path, &input.payee);
        log::info!(
            "ldk routing: {} hop(s), {} msat fee, blinded tail: {}",
            path.hops.len(),
            path.fee_msat(),
            path.blinded_tail.is_some()
        );
        Ok(RouteOutput { path })
    }
}

/// LDK routes to a one-hop blinded path (the recipient is its own
/// introduction node) with a CLTV delta of 0 for the final hop: it takes
/// such paths as stand-ins whose payinfo is a placeholder. Core Lightning
/// puts its minimum final CLTV there and enforces it ("Expiry cltv too
/// soon"), so the path's `cltv_expiry_delta` is added to the final hop.
pub fn one_hop_blinded_cltv(path: &mut lampo_common::ldk::routing::router::Path, payee: &Payee) {
    let Payee::Blinded { paths, .. } = payee else {
        return;
    };
    let Some(tail) = path.blinded_tail.as_ref().filter(|t| t.hops.len() == 1) else {
        return;
    };
    let Some((payinfo, _)) = paths
        .iter()
        .find(|(_, blinded)| blinded.blinding_point == tail.blinding_point)
    else {
        return;
    };
    if let Some(last) = path.hops.last_mut() {
        last.cltv_expiry_delta += payinfo.cltv_expiry_delta as u32;
    }
}

#[cfg(test)]
mod one_hop_tests {
    use std::str::FromStr;

    use lampo_common::ldk::blinded_path::{BlindedHop, BlindedPath, IntroductionNode};
    use lampo_common::ldk::ln::features::{BlindedHopFeatures, ChannelFeatures, NodeFeatures};
    use lampo_common::ldk::offers::invoice::BlindedPayInfo;
    use lampo_common::ldk::routing::router::{BlindedTail, Path, RouteHop};
    use lampo_common::secp256k1::PublicKey;

    use super::*;

    fn pk(hex: &str) -> PublicKey {
        PublicKey::from_str(hex).unwrap()
    }

    #[test]
    fn one_hop_blinded_paths_get_their_cltv() {
        let bob = pk("0324653eac434488002cc06bbfb7f10fe18991e35f9fe4302dbea6d2353dc0ab1c");
        let point = pk("027f31ebc5462c1fdce1b737ecff52d37d75dea43ce11c74d25aa297165faa2007");
        let payinfo = BlindedPayInfo {
            fee_base_msat: 0,
            fee_proportional_millionths: 0,
            htlc_minimum_msat: 0,
            htlc_maximum_msat: u64::MAX,
            cltv_expiry_delta: 10,
            features: BlindedHopFeatures::empty(),
        };
        let blinded = |hops: usize| BlindedPath {
            introduction_node: IntroductionNode::NodeId(bob),
            blinding_point: point,
            blinded_hops: vec![
                BlindedHop {
                    blinded_node_id: point,
                    encrypted_payload: vec![1],
                };
                hops
            ],
        };
        let path = |shadow: u32, hops: usize| Path {
            hops: vec![RouteHop {
                pubkey: bob,
                node_features: NodeFeatures::empty(),
                short_channel_id: 1,
                channel_features: ChannelFeatures::empty(),
                fee_msat: 12_000,
                // LDK's 0 for the one-hop path, plus its random offset.
                cltv_expiry_delta: shadow,
                maybe_announced_channel: true,
            }],
            blinded_tail: Some(BlindedTail {
                hops: blinded(hops).blinded_hops,
                blinding_point: point,
                excess_final_cltv_expiry_delta: shadow,
                final_value_msat: 12_000,
            }),
        };
        let payee = |hops: usize| Payee::Blinded {
            paths: vec![(payinfo.clone(), blinded(hops))],
            features: None,
        };

        let mut one = path(6, 1);
        one_hop_blinded_cltv(&mut one, &payee(1));
        assert_eq!(one.hops[0].cltv_expiry_delta, 16);

        // Longer paths already carry the payinfo's delta.
        let mut two = path(46, 2);
        one_hop_blinded_cltv(&mut two, &payee(2));
        assert_eq!(two.hops[0].cltv_expiry_delta, 46);
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use lampo_common::conf::Network;
    use lampo_common::secp256k1::PublicKey;

    use super::*;

    fn pk(hex: &str) -> PublicKey {
        PublicKey::from_str(hex).unwrap()
    }

    fn context() -> Arc<NetworkContext> {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "barq-ldk-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        NetworkContext::new(Network::Regtest, dir, None, None).unwrap()
    }

    #[test]
    fn direct_peer_without_gossip() {
        // With first hops the router can pay a direct peer even on an
        // empty graph.
        let us = pk("02eec7245d6b7d2ccb30380bfbe2a3648cd7a942653f5aa340edcea1f283686619");
        let peer = pk("0324653eac434488002cc06bbfb7f10fe18991e35f9fe4302dbea6d2353dc0ab1c");
        let strategy = LDKRoutingStrategy::new(context());
        let input = RouteInput {
            src_pubkey: us,
            payee: Payee::Clear {
                node_id: peer,
                route_hints: vec![],
                features: None,
            },
            amount_msat: 10_000,
            final_cltv_delta: 18,
            first_hops: vec![FirstHop {
                peer_id: peer,
                scid: (103u64 << 40) | (1 << 16),
                cln_channel: "103x1x0".to_owned(),
                capacity_sat: 1_000_000,
                spendable_msat: 500_000_000,
                htlc_minimum_msat: 1,
                htlc_maximum_msat: 990_000_000,
                is_public: true,
            }],
            expiry_time: None,
            strategy: None,
            excluded_channels: vec![],
        };
        let output = strategy.route(&input).unwrap();
        assert_eq!(output.path.hops.len(), 1);
        assert_eq!(output.path.hops[0].pubkey, peer);
        assert_eq!(output.path.hops[0].fee_msat, 10_000);
        // LDK adds a random shadow offset to the final CLTV for privacy.
        assert!(output.path.hops[0].cltv_expiry_delta >= 18);
    }

    #[test]
    fn unknown_destination_fails() {
        let us = pk("02eec7245d6b7d2ccb30380bfbe2a3648cd7a942653f5aa340edcea1f283686619");
        let peer = pk("0324653eac434488002cc06bbfb7f10fe18991e35f9fe4302dbea6d2353dc0ab1c");
        let dest = pk("027f31ebc5462c1fdce1b737ecff52d37d75dea43ce11c74d25aa297165faa2007");
        let strategy = LDKRoutingStrategy::new(context());
        let input = RouteInput {
            src_pubkey: us,
            payee: Payee::Clear {
                node_id: dest,
                route_hints: vec![],
                features: None,
            },
            amount_msat: 10_000,
            final_cltv_delta: 18,
            first_hops: vec![FirstHop {
                peer_id: peer,
                scid: 1,
                cln_channel: "0x0x1".to_owned(),
                capacity_sat: 1_000_000,
                spendable_msat: 500_000_000,
                htlc_minimum_msat: 1,
                htlc_maximum_msat: 990_000_000,
                is_public: true,
            }],
            expiry_time: None,
            strategy: None,
            excluded_channels: vec![],
        };
        assert!(strategy.route(&input).is_err());
    }
}
