use crate::RouteInput::{
    Edge as RouteInputEdge, NetworkGraph as RouteInputNetworkGraph, Node as RouteInputNode,
};
use lightning::{
    routing::{
        gossip::{NetworkGraph as LdkNetworkGraph, NodeId},
        network_graph::{DirectedChannelAnnouncement, EdgeInfo, NodeAnnouncementInfo, NodeInfo},
    },
    util::ser::{Readable, Writeable},
};
use std::collections::HashMap;

/// Convert a `RouteInput::NetworkGraph` to a `NetworkGraph` from Rust Lightning
pub fn convert_network_graph(input_graph: &RouteInputNetworkGraph) -> LdkNetworkGraph {
    unimplemented!("Convert RouteInputNetworkGraph to LdkNetworkGraph")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RouteInput::{
        Edge as RouteInputEdge, NetworkGraph as RouteInputNetworkGraph, Node as RouteInputNode,
    };

    #[test]
    fn test_convert_network_graph() {
        unimplemented!("Test convert_network_graph")
    }
}
