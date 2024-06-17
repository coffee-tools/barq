// pub mod network_graph_conversion;

use core::ops::Deref;

use lightning::{
    ln::channelmanager::ChannelDetails,
    routing::{
        gossip::NetworkGraph as LdkNetworkGraph,
        router::{find_route, Route},
        scoring::ScoreLookUp,
    },
    util::{
        logger::{Logger, Record},
        ser::Writeable,
    },
};

use crate::{
    graph::NetworkGraph,
    strategy::{RouteInput, RouteOutput, Strategy},
};

pub struct LdkRouter<L, S>
where
    L: Deref,
    S: ScoreLookUp,
    L::Target: Logger,
{
    logger: L,
    scorer: S,
}

impl<L, S> LdkRouter<L, S>
where
    L: Deref,
    S: ScoreLookUp,
    L::Target: Logger,
{
    pub fn new(logger: L, scorer: S) -> Self {
        Self { logger, scorer }
    }

    fn convert_to_ldk_network_graph(graph: &NetworkGraph) -> LdkNetworkGraph<L> {
        // TODO: Convert NetworkGraph to LDK NetworkGraph
        unimplemented!()
    }

    fn construct_route_params(input: &RouteInput) -> lightning::routing::router::RouteParameters {
        // TODO: Construct RouteParameters from RouteInput
        unimplemented!()
    }

    fn convert_route_to_output(route: Route) -> RouteOutput {
        // TODO: Convert LDK Route to RouteOutput
        unimplemented!()
    }
}

impl<L, S> Strategy for LdkRouter<L, S>
where
    L: Deref,
    S: ScoreLookUp,
    L::Target: Logger,
{
    fn can_apply(&self, _input: &RouteInput) -> bool {
        // TODO: Implement the logic to check if the strategy can be applied to the given input
        true
    }

    fn route(&self, input: &RouteInput) -> RouteOutput {
        let ldk_graph = Self::convert_to_ldk_network_graph(&input.graph);
        let route_params = Self::construct_route_params(input);

        // TODO: Implement the logic to find route using LDK using `find_route`

        unimplemented!("Implement the logic to find route using LDK using `find_route`");
    }
}
