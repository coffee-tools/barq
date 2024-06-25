use core::ops::Deref;
use std::str::FromStr;

use bitcoin::Network;
use lightning::{
    ln::{
        channelmanager::ChannelDetails,
        msgs::{ChannelAnnouncement, ChannelUpdate, UnsignedChannelAnnouncement},
    },
    routing::{
        gossip::{
            NetworkGraph as LdkNetworkGraph, NetworkUpdate, NetworkUpdate::ChannelUpdateMessage,
        },
        router::{find_route, Route, RouteParameters},
        scoring::{
            ProbabilisticScorer, ProbabilisticScoringDecayParameters,
            ProbabilisticScoringFeeParameters, ScoreLookUp,
        },
    },
    util::{
        logger::{Logger, Record},
        ser::Writeable,
    },
};
use secp256k1::PublicKey;

use crate::{
    graph::NetworkGraph,
    strategy::{RouteInput, RouteOutput, Strategy},
};

pub mod logger;

pub struct LdkRouter<L>
where
    L: Deref,
    L::Target: Logger,
{
    logger: L,
}

impl<L> LdkRouter<L>
where
    L: Deref,
    L::Target: Logger,
{
    pub fn new(logger: L) -> Self {
        Self { logger }
    }

    fn convert_to_ldk_network_graph(&self, graph: &NetworkGraph) -> LdkNetworkGraph<L> {
        // let network = Network::Bitcoin;
        // let mut ldk_graph = LdkNetworkGraph::new(network, self.logger);

        for (_, edge) in &graph.edges {
            // let channel = ChannelAnnouncement {
            //     node_signature_1: [0; 64],
            //     node_signature_2: [0; 64],
            //     bitcoin_signature_1: [0; 64],
            //     bitcoin_signature_2: [0; 64],
            //     contents: UnsignedChannelAnnouncement {
            //         features: Default::default(),
            //         chain_hash: Default::default(),
            //         short_channel_id: Default::default(),
            //         node_id_1: Default::default(),
            //         node_id_2: Default::default(),
            //         bitcoin_key_1: Default::default(),
            //         bitcoin_key_2: Default::default(),
            //     },
            // };

            // ldk_graph
            //     .update_channel_from_announcement(&channel, &None)
            //     .unwrap(); // TODO: Handle error
        }

        // ldk_graph

        unimplemented!()
    }

    fn construct_route_params(input: &RouteInput) -> RouteParameters {
        // TODO: Construct RouteParameters from RouteInput
        unimplemented!()
    }

    fn convert_route_to_output(route: Route) -> RouteOutput {
        // let total_fees = route.route_params.unwrap().final_value_msat; // TODO: Handle unwrap
        // let mut hops = Vec::new();
        // let route = route.paths[0]; // TODO: Why is this a vector?
        // for hop in route.hops {
        //     let hop = hop.short_channel_id.to_string();
        //     hops.push(hop);
        // }

        // TODO: Convert LDK Route to RouteOutput
        unimplemented!()
    }
}

impl<L> Strategy for LdkRouter<L>
where
    L: Deref + Logger + Copy,
    L::Target: Logger,
{
    fn can_apply(&self, _input: &RouteInput) -> bool {
        // TODO: Implement the logic to check if the strategy can be applied to the given input
        true
    }

    fn route(&self, input: &RouteInput) -> RouteOutput {
        let our_pubkey = PublicKey::from_str(&input.source).unwrap();
        let route_params = Self::construct_route_params(input);
        let ldk_graph = self.convert_to_ldk_network_graph(&input.graph);
        let scorer = ProbabilisticScorer::new(
            ProbabilisticScoringDecayParameters::default(),
            &ldk_graph,
            self.logger,
        );
        let random_seed_bytes = [0; 32];

        let route = find_route(
            &our_pubkey,
            &route_params,
            &ldk_graph,
            None,
            &self.logger,
            &scorer,
            &ProbabilisticScoringFeeParameters::default(),
            &random_seed_bytes,
        )
        .unwrap(); // TODO: Handle error

        Self::convert_route_to_output(route)
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
        to write test cases for the `route` method of `LdkRouter`
         */
    }
}
