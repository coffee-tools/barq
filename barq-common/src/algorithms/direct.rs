use crate::{
    graph::NetworkGraph,
    strategy::{RouteHop, RouteInput, RouteOutput, Strategy},
};
use std::collections::HashMap;

pub struct Direct;

impl Default for Direct {
    fn default() -> Self {
        Self::new()
    }
}

impl Direct {
    pub fn new() -> Self {
        Direct
    }
}

impl Strategy for Direct {
    fn can_apply(&self, input: &RouteInput) -> bool {
        // TODO: Implement the logic to check if the strategy can be applied to the given input
        let source = input.src_pubkey.clone();
        let node = input.graph.get_node(&source).expect("Error is strategy.rs");

        for channel in &node.channels {
            let edge = input.graph.get_edge(channel).expect("Error in strategy.rs");
            if input.dest_pubkey == edge.node1.clone() || input.dest_pubkey == edge.node2 {
                return true;
            }
        }
        true
    }

    fn route(&self, input: &RouteInput) -> Result<RouteOutput, String> {
        // TODO: Implement the routing logic
        let source = input.src_pubkey.clone();
        let node = input
            .graph
            .get_node(&source)
            .ok_or_else(|| format!("Cannot retrieve node for id {}", &source))?;

        let mut path: Vec<RouteHop> = Vec::<RouteHop>::new();

        for channel in &node.channels {
            let edge = input
                .graph
                .get_edge(channel)
                .ok_or_else(|| format!("Error retrieving edge for channel {}", channel))?;

            if input.dest_pubkey == edge.node1.clone() || input.dest_pubkey == edge.node2.clone() {
                let delay = 9;
                let hop = RouteHop::new(
                    input.dest_pubkey.clone(),
                    channel.clone(),
                    delay,
                    input.amount_msat,
                );
                path.push(hop);
            }
        }

        Ok(RouteOutput { path: path })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        graph::{Edge, Node},
        strategy::Router,
    };

    #[test]
    fn test_direct_routing() {
        let mut graph = NetworkGraph::new();
        graph.add_node(Node::new("A"));
        graph.add_node(Node::new("B"));
        graph.add_edge(Edge::new("channel", "A", "B", 100, 6, 1, 10));
        let mut strategies: Vec<Box<dyn Strategy>> = Vec::new();
        strategies.push(Box::new(Direct::new()));
        let router = Router::new(strategies);
        let input = RouteInput {
            src_pubkey: "A".to_string(),
            dest_pubkey: "B".to_string(),
            cltv: 9,
            amount_msat: 100,
            graph,
        };
        let output = router.execute(&input).expect("Direct Routing Failed");
        let mut route_path: Vec<RouteHop> = Vec::<RouteHop>::new();
        let hop = RouteHop::new("B".to_string(), "channel".to_string(), 9, 100);
        route_path.push(hop);
        assert_eq!(output.path, route_path);
        // TODO: complete writing tests
    }
}
