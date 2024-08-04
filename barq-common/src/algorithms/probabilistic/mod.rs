use core::ops::Deref;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::Result;

use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::ldk::routing::gossip::NetworkGraph as LdkNetworkGraph;
use lampo_common::ldk::routing::router::{find_route, Route, RouteParameters};
use lampo_common::ldk::routing::scoring::FixedPenaltyScorer;
use lampo_common::ldk::util::logger::Logger;
use lampo_common::utils::logger::LampoLogger;

use crate::graph::NetworkGraph;
use crate::strategy::{RouteHop, RouteInput, RouteOutput, Strategy};

/// A routing strategy that uses the LDK crates to find the best route.
pub struct LDKRoutingStrategy<L>
where
    L: Deref,
    L::Target: Logger,
{
    logger: L,
}

impl Default for LDKRoutingStrategy<Arc<LampoLogger>> {
    fn default() -> Self {
        Self::new(Arc::new(LampoLogger::new()))
    }
}

impl<L> LDKRoutingStrategy<L>
where
    L: Deref,
    L::Target: Logger,
{
    pub fn new(logger: L) -> Self {
        Self { logger }
    }

    fn convert_to_ldk_network_graph(&self, graph: &dyn NetworkGraph) -> LdkNetworkGraph<L> {
        for channel in graph.get_channels() {
            // TODO: Convert Channel to LDK ChannelAnnouncement
            let _channel = channel;
        }

        unimplemented!("convert_to_ldk_network_graph not implemented yet.")
    }

    fn construct_route_params(input: &RouteInput) -> RouteParameters {
        // TODO: Implement the logic to construct RouteParameters from the given input
        let _input = input;
        unimplemented!("construct_route_params not implemented yet.")
    }

    fn convert_route_to_output(route: Route) -> RouteOutput {
        let path = route.paths.first().expect("No LDK path available");
        let mut amt_to_forward = 0;
        let mut delay = 0;

        let output_path: Vec<RouteHop> = path
            .hops
            .iter()
            .rev()
            .map(|hop| {
                amt_to_forward += hop.fee_msat;
                delay += hop.cltv_expiry_delta;

                RouteHop::new(
                    hop.pubkey.to_string(),
                    hop.short_channel_id.to_string(),
                    delay,
                    amt_to_forward,
                )
            })
            .collect();

        RouteOutput {
            path: output_path.into_iter().rev().collect(),
        }
    }
}

impl<L> Strategy for LDKRoutingStrategy<L>
where
    L: Deref,
    L::Target: Logger,
{
    /// Determines if the LDK routing strategy can be applied to the given
    /// input.
    ///
    /// This method checks if the network graph has the peer-to-peer information
    /// required for LDK routing.
    fn can_apply(&self, input: &RouteInput) -> Result<bool> {
        if input.graph.has_p2p_info() {
            return Ok(true);
        }
        log::warn!(
            "The network graph does not have peer-to-peer information required for LDK routing"
        );
        Ok(false)
    }

    fn route(&self, input: &RouteInput) -> Result<RouteOutput> {
        let our_node_pubkey = PublicKey::from_str(&input.src_pubkey)
            .map_err(|_| anyhow::anyhow!("Failed to parse source pubkey"))?;
        let route_params = Self::construct_route_params(input);
        let ldk_graph = self.convert_to_ldk_network_graph(input.graph.as_ref());
        // TODO: What scorer should we use?
        // See: https://github.com/lightningdevkit/rust-lightning/blob/main/lightning/src/routing/scoring.rs#L10-L13
        let scorer = FixedPenaltyScorer::with_penalty(0);
        // TODO: Implement the logic to generate random seed bytes
        let random_seed_bytes = [0; 32];

        let route = find_route(
            &our_node_pubkey,
            &route_params,
            &ldk_graph,
            None,
            self.logger.deref(),
            &scorer,
            &(),
            &random_seed_bytes,
        )
        .map_err(|e| anyhow::anyhow!("Failed to find route: {:?}", e))?;

        Ok(Self::convert_route_to_output(route))
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_route() {
        /*
        Use:
        - https://github.com/lightningdevkit/rust-lightning/blob/main/lightning/src/routing/test_utils.rs#L185
        - https://github.com/lightningdevkit/rust-lightning/blob/main/lightning/src/routing/router.rs#L3428
        to write test cases for the `route` method of `LDKRoutingStrategy`
         */
    }
}
