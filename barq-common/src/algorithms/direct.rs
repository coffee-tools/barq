use anyhow::Result;

use crate::strategy::{RouteHop, RouteInput, RouteOutput, Strategy};

/// A routing strategy that attempts to find a direct route from the source to
/// the destination.
///
/// The `Direct` strategy checks if the destination node is directly connected
/// to the source node through any of the channels. If such a direct connection
/// exists, it constructs a route with that single hop.
pub struct Direct;

impl Direct {
    pub fn new() -> Self {
        Direct
    }
}

impl Default for Direct {
    fn default() -> Self {
        Direct::new()
    }
}

impl Strategy for Direct {
    /// Determines if the Direct routing strategy can be applied to the given
    /// input.
    ///
    /// This method checks if the destination node is directly connected to the
    /// source node within the network graph.
    fn can_apply(&self, input: &RouteInput) -> Result<bool> {
        let source = input.src_pubkey.clone();
        let node = input
            .graph
            .get_node(&source)
            .ok_or_else(|| anyhow::anyhow!("Failed to retrieve source node from graph"))?;

        // Check if the destination is directly connected to the source
        for channel in &node.channels {
            if channel.node1 == input.dest_pubkey || channel.node2 == input.dest_pubkey {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn set_network(&mut self, _network: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// Routes the payment directly from the source to the destination node.
    ///
    /// This method constructs a route with a single hop if a direct connection
    /// exists between the source and destination nodes.
    fn route(&self, input: &RouteInput) -> Result<RouteOutput> {
        let source = input.src_pubkey.clone();
        let node = input
            .graph
            .get_node(&source)
            .ok_or_else(|| anyhow::anyhow!("Failed to retrieve node from graph"))?;

        let channels = node
            .channels
            .iter()
            .filter(|ch| ch.node1 == input.dest_pubkey || ch.node2 == input.dest_pubkey)
            .collect::<Vec<_>>();

        if channels.is_empty() {
            anyhow::bail!("No channel with `{}` found", input.dest_pubkey);
        }

        let channels = channels
            .iter()
            .filter(|c| c.capacity >= input.amount_msat)
            .collect::<Vec<_>>();
        let Some(channel) = channels.first() else {
            anyhow::bail!(
                "No channel with capacity `{}` with peer `{}` found",
                input.amount_msat,
                input.dest_pubkey
            );
        };

        let hop = RouteHop::new(
            input.dest_pubkey.clone(),
            channel.short_channel_id.clone(),
            input.cltv as u32,
            // FIXME: Double check for this?
            input.amount_msat
                + channel.base_fee_millisatoshi
                + (channel.fee_per_millionth * input.amount_msat),
        );

        Ok(RouteOutput { path: vec![hop] })
    }
}
