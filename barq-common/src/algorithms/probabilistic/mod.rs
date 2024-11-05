use core::ops::Deref;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::Result;
use reqwest::blocking;

use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::conf::Network;
use lampo_common::ldk::ln::msgs::ChannelAnnouncement;
use lampo_common::ldk::routing::gossip::NetworkGraph as LdkNetworkGraph;
use lampo_common::ldk::routing::router::{find_route, PaymentParameters, Route, RouteParameters};
use lampo_common::ldk::routing::scoring::{
    ProbabilisticScorer, ProbabilisticScoringDecayParameters, ProbabilisticScoringFeeParameters,
};
use lampo_common::ldk::util::ser::Readable;
use lampo_common::utils::logger::LampoLogger;
use lightning_rapid_gossip_sync::RapidGossipSync;

use crate::graph::NetworkGraph;
use crate::strategy::{RouteHop, RouteInput, RouteOutput, Strategy};

/// A routing strategy that uses the LDK crates to find the best route.
pub struct LDKRoutingStrategy {
    logger: Arc<LampoLogger>,
    network: Network,
    root_path: String,
}

impl LDKRoutingStrategy {
    pub fn new(network: Network, root_path: String) -> Self {
        Self {
            logger: Arc::new(LampoLogger::new()),
            network,
            root_path,
        }
    }

    fn convert_to_ldk_network_graph(
        &self,
        graph: &dyn NetworkGraph,
    ) -> anyhow::Result<LdkNetworkGraph<Arc<LampoLogger>>> {
        let ldkgraph = LdkNetworkGraph::new(self.network.clone(), self.logger.clone());

        for channel in graph.get_channels() {
            // FIXME: we need to set the annouce message insie the channel struct
            if let Some(msg) = channel.channel_announcement.clone() {
                let channel_ann = ChannelAnnouncement::read(&mut msg.as_slice())
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                ldkgraph
                    .update_channel_from_announcement_no_lookup(&channel_ann)
                    .map_err(|err| anyhow::anyhow!("{:?}", err))?;
            }
        }

        Ok(ldkgraph)
    }

    fn construct_route_params(input: &RouteInput) -> RouteParameters {
        // SAFETY: safe to unwrap because it should be a valid pub key
        let payment_params = PaymentParameters::from_node_id(
            PublicKey::from_str(&input.dest_pubkey).unwrap(),
            input.cltv as u32,
        );
        RouteParameters::from_payment_params_and_value(payment_params, input.amount_msat)
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

    fn rapid_gossip_sync_network(
        &self,
        network: Network,
    ) -> Result<LdkNetworkGraph<Arc<LampoLogger>>> {
        let graph = LdkNetworkGraph::new(network, self.logger.clone());
        let rapid_sync = RapidGossipSync::new(&graph, self.logger.clone());

        // Download the snapshot data
        // For more information, see: https://docs.rs/lightning-rapid-gossip-sync/latest/lightning_rapid_gossip_sync/#getting-started
        let rapid_gossip_sync_url = match network {
            Network::Bitcoin => "https://rapidsync.lightningdevkit.org/snapshot/0",
            Network::Testnet => "https://rapidsync.lightningdevkit.org/testnet/snapshot/0",
            _ => {
                return Err(anyhow::anyhow!(
                    "Rapid gossip sync is not supported for the given network: {:?}. Please use either Bitcoin or Testnet",
                    network
                ))
            }
        };
        let response = blocking::get(rapid_gossip_sync_url)?;
        let snapshot_contents = response.bytes()?;

        rapid_sync
            .update_network_graph(&snapshot_contents)
            .map_err(|e| {
                anyhow::anyhow!("Failed to update network graph with snapshot: {:?}", e)
            })?;

        Ok(graph)
    }
}

impl Strategy for LDKRoutingStrategy {
    /// Determines if the LDK routing strategy can be applied to the given
    /// input.
    ///
    /// This method checks if the network graph either has:
    /// - Peer-to-peer information
    /// - Rapid gossip sync enabled
    fn can_apply(&self, input: &RouteInput) -> Result<bool> {
        if input.graph.has_p2p_info() || input.use_rapid_gossip_sync {
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

        let ldk_graph = if input.use_rapid_gossip_sync {
            self.rapid_gossip_sync_network(input.network)?
        } else {
            self.convert_to_ldk_network_graph(input.graph.as_ref())?
        };

        // FIXME: We should check if there is a better way for this.
        let parms = ProbabilisticScoringDecayParameters::default();
        let feeparams = ProbabilisticScoringFeeParameters::default();
        let scorer = ProbabilisticScorer::new(parms, &ldk_graph, self.logger.clone());

        // FIXME: Implement the logic to generate random seed bytes
        let random_seed_bytes = [0; 32];

        let route = find_route(
            &our_node_pubkey,
            &route_params,
            &ldk_graph,
            None,
            self.logger.deref(),
            &scorer,
            &feeparams,
            &random_seed_bytes,
        )
        // FIXME: we are losing context, we should return an better error for the plugin
        .map_err(|e| anyhow::anyhow!("Failed to find route: {:?}", e))?;

        Ok(Self::convert_route_to_output(route))
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use lampo_common::ldk::util::logger::{Logger, Record};

    #[test]
    fn test_rapid_gossip_sync_network_sanity() {
        let network = Network::Bitcoin;
        let strategy = LDKRoutingStrategy::new(network.clone(), "/tmp".to_string());
        let result = strategy.rapid_gossip_sync_network(network);

        assert!(
            result.is_ok(),
            "Failed to create a network graph using rapid gossip sync: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_rapid_gossip_sync_network_testnet() {
        let network = Network::Testnet;
        let strategy = LDKRoutingStrategy::new(network.clone(), "/tmp".to_string());
        let result = strategy.rapid_gossip_sync_network(network);

        assert!(
            result.is_ok(),
            "Failed to create a network graph using rapid gossip sync: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_rapid_gossip_sync_network_not_empty() {
        let network = Network::Testnet;
        let strategy = LDKRoutingStrategy::new(network.clone(), "/tmp".to_string());
        let graph = strategy.rapid_gossip_sync_network(network).unwrap();

        let read_only_graph = graph.read_only();
        let channels = read_only_graph.channels();
        assert!(
            !channels.is_empty(),
            "No channels found in the network graph"
        );

        let nodes = read_only_graph.nodes();
        assert!(!nodes.is_empty(), "No nodes found in the network graph");
    }
}
