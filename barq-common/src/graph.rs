use std::collections::HashMap;

use anyhow::Result;
use petgraph::{
    dot::{Config, Dot},
    graph::{DiGraph, NodeIndex},
};
use serde::{Deserialize, Serialize};

/// Represents a node in the network graph.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub alias: Option<String>,
    pub channels: Vec<Edge>,
}

impl Node {
    /// Creates a new node.
    pub fn new(id: &str) -> Self {
        Node {
            id: id.to_string(),
            alias: None,
            channels: vec![],
        }
    }

    /// Sets the alias of the node.
    pub fn set_alias(&mut self, alias: &str) {
        self.alias = Some(alias.to_string());
    }

    /// Adds a channel to the node.
    pub fn add_channel(&mut self, channel: &Edge) {
        self.channels.push(channel.clone());
    }
}

/// Represents an edge (channel) between two nodes in the network graph.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct Edge {
    pub id: String,
    pub node1: String,
    pub node2: String,
    pub capacity: u64,
    pub delay: u64,
    pub base_fee_millisatoshi: u64,
    pub fee_per_millionth: u64,
}

impl Edge {
    /// Creates a new edge (channel).
    pub fn new(
        id: &str,
        node1: &str,
        node2: &str,
        capacity: u64,
        delay: u64,
        base_fee_millisatoshi: u64,
        fee_per_millionth: u64,
    ) -> Self {
        Edge {
            id: id.to_string(),
            node1: node1.to_string(),
            node2: node2.to_string(),
            capacity,
            delay,
            base_fee_millisatoshi,
            fee_per_millionth,
        }
    }

    /// Sets the capacity of the edge.
    pub fn set_capacity(&mut self, capacity: u64) {
        self.capacity = capacity;
    }
}

/// Represents the network graph of nodes and edges.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NetworkGraph {
    nodes: HashMap<String, Node>,
    edges: HashMap<String, Edge>,
}

impl Default for NetworkGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkGraph {
    /// Creates a new, empty network graph.
    pub fn new() -> Self {
        NetworkGraph {
            nodes: HashMap::new(),
            edges: HashMap::new(),
        }
    }

    /// Adds a node to the network graph.
    pub fn add_node(&mut self, node: Node) {
        self.nodes.insert(node.id.clone(), node);
    }

    /// Adds an edge (channel) to the network graph.
    pub fn add_edge(&mut self, edge: Edge) {
        self.edges.insert(edge.id.clone(), edge.clone());
        if let Some(source_node) = self.nodes.get_mut(&edge.clone().node1) {
            source_node.add_channel(&edge);
        }
    }

    /// Gets a reference to a node by its ID.
    pub fn get_node(&self, node_id: &str) -> Option<&Node> {
        self.nodes.get(node_id)
    }

    /// Gets a reference to an edge by its ID.
    pub fn get_edge(&self, edge_id: &str) -> Option<&Edge> {
        self.edges.get(edge_id)
    }

    /// Gets all nodes in the network graph.
    pub fn get_all_nodes(&self) -> Vec<&Node> {
        self.nodes.values().collect()
    }

    /// Gets all edges in the network graph.
    pub fn get_all_edges(&self) -> Vec<&Edge> {
        self.edges.values().collect()
    }

    // TODO: Add methods for updating nodes and edges.
    // TODO: Add methods for removing nodes and edges.

    /// Returns a DOT representation of the network graph.
    pub fn to_dot(&self) -> Result<String> {
        let mut graph = DiGraph::new();
        let mut node_indices: HashMap<String, NodeIndex> = HashMap::new();

        for node in self.nodes.values() {
            let node_index = graph.add_node(node.id.clone());
            node_indices.insert(node.id.clone(), node_index);
        }

        for edge in self.edges.values() {
            let source_index = node_indices.get(&edge.node1).ok_or_else(|| {
                anyhow::anyhow!("Failed to get node index for node {}", edge.node1)
            })?;
            let destination_index = node_indices.get(&edge.node2).ok_or_else(|| {
                anyhow::anyhow!("Failed to get node index for node {}", edge.node2)
            })?;
            graph.add_edge(*source_index, *destination_index, edge.capacity);
        }

        Ok(format!(
            "{:?}",
            Dot::with_config(&graph, &[Config::EdgeNoLabel])
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_node() {
        let node = Node::new("node1");
        assert_eq!(node.id, "node1");
        assert!(node.alias.is_none());
        assert!(node.channels.is_empty());
    }

    #[test]
    fn test_set_node_alias() {
        let mut node = Node::new("node1");
        node.set_alias("Node 1 Alias");
        assert_eq!(node.alias, Some("Node 1 Alias".to_string()));
    }

    #[test]
    fn test_add_channel_to_node() {
        let mut node = Node::new("node1");
        let edge = Edge::new("channel1", "node1", "node2", 1000, 6, 1, 10);
        node.add_channel(&edge);
        assert_eq!(node.channels.len(), 1);
    }

    #[test]
    fn test_create_edge() {
        let edge = Edge::new("channel1", "node1", "node2", 1000, 6, 1, 10);
        assert_eq!(edge.id, "channel1");
        assert_eq!(edge.node1, "node1");
        assert_eq!(edge.node2, "node2");
        assert_eq!(edge.capacity, 1000);
    }

    #[test]
    fn test_set_edge_capacity() {
        let mut edge = Edge::new("channel1", "node1", "node2", 1000, 6, 1, 10);
        edge.set_capacity(2000);
        assert_eq!(edge.capacity, 2000);
    }

    #[test]
    fn test_add_node_to_graph() {
        let mut graph = NetworkGraph::new();
        let node = Node::new("node1");
        graph.add_node(node);
        assert!(graph.get_node("node1").is_some());
    }

    #[test]
    fn test_add_edge_to_graph() {
        let mut graph = NetworkGraph::new();
        graph.add_node(Node::new("node1"));
        graph.add_node(Node::new("node2"));

        let edge = Edge::new("channel1", "node1", "node2", 1000, 6, 1, 10);
        graph.add_edge(edge);

        assert!(graph.get_edge("channel1").is_some());
        assert_eq!(graph.get_node("node1").unwrap().channels.len(), 1);
    }
}
