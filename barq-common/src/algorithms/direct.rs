use anyhow::Result;

use lampo_common::ldk::ln::features::{ChannelFeatures, NodeFeatures};
use lampo_common::ldk::routing::router::{Path, RouteHop};

use crate::strategy::{Payee, RouteInput, RouteOutput, Strategy};

/// A routing strategy that attempts to find a direct route from the source to
/// the destination.
///
/// The `Direct` strategy checks if the destination node is directly connected
/// to the source node through any of our channels. If such a direct connection
/// exists, it constructs a route with that single hop.
#[derive(Default)]
pub struct Direct;

impl Direct {
    pub fn new() -> Self {
        Direct
    }
}

impl Strategy for Direct {
    /// The destination must be known (BOLT11) and be one of our peers.
    fn can_apply(&self, input: &RouteInput) -> Result<bool> {
        let Some(dest) = input.payee.node_id() else {
            return Ok(false);
        };
        Ok(input.first_hops.iter().any(|hop| hop.peer_id == dest))
    }

    /// Routes the payment directly from the source to the destination node.
    fn route(&self, input: &RouteInput) -> Result<RouteOutput> {
        let dest = match &input.payee {
            Payee::Clear { node_id, .. } => *node_id,
            Payee::Blinded { .. } => {
                anyhow::bail!("The direct strategy cannot pay to a blinded path")
            }
        };

        let channels: Vec<_> = input
            .first_hops
            .iter()
            .filter(|hop| hop.peer_id == dest)
            .filter(|hop| !input.excluded_channels.contains(&hop.scid))
            .collect();
        if channels.is_empty() {
            anyhow::bail!("No channel with `{dest}` found");
        }

        let Some(channel) = channels.iter().find(|hop| {
            hop.spendable_msat >= input.amount_msat
                && hop.htlc_minimum_msat <= input.amount_msat
                && hop.htlc_maximum_msat >= input.amount_msat
        }) else {
            anyhow::bail!(
                "No channel with peer `{dest}` can carry {} msat",
                input.amount_msat
            );
        };

        let hop = RouteHop {
            pubkey: dest,
            node_features: NodeFeatures::empty(),
            short_channel_id: channel.scid,
            channel_features: ChannelFeatures::empty(),
            fee_msat: input.amount_msat,
            cltv_expiry_delta: input.final_cltv_delta,
            maybe_announced_channel: channel.is_public,
        };

        Ok(RouteOutput {
            path: Path {
                hops: vec![hop],
                blinded_tail: None,
            },
        })
    }
}
