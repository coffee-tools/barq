use anyhow::Result;

use crate::strategy::{RouteHop, RouteInput, RouteOutput, Strategy};

const DEFAULT_DELAY: u64 = 9;

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

        let mut path: Vec<RouteHop> = Vec::<RouteHop>::new();

        for edge in &node.channels {
            if input.dest_pubkey == edge.node1.clone() || input.dest_pubkey == edge.node2.clone() {
                let hop = RouteHop::new(
                    input.dest_pubkey.clone(),
                    edge.id.clone(),
                    DEFAULT_DELAY,
                    input.amount_msat,
                );
                path.push(hop);
            }
        }

        Ok(RouteOutput { path })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Edge, NetworkGraph, Node};
    use crate::strategy::Router;

    #[test]
    fn test_direct_routing() {
        let mut graph = NetworkGraph::new();
        graph.add_node(Node::new("A"));
        graph.add_node(Node::new("B"));
        graph.add_edge(Edge::new("channel", "A", "B", 100, 6, 1, 10));
        let strategies: Vec<Box<dyn Strategy>> = vec![Box::new(Direct::new())];
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
        let edge = Edge::new("channel", "A", "B", 100, 6, 1, 10);
        let hop = RouteHop::new("B".to_string(), edge.id, 9, 100);
        route_path.push(hop);
        assert_eq!(output.path, route_path);
    }
}
