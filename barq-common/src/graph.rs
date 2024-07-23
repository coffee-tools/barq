use serde::{Deserialize, Serialize};

/// Represents a node in the network graph.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub alias: Option<String>,
    pub channels: Vec<Channel>,
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
    pub fn add_channel(&mut self, channel: &Channel) {
        self.channels.push(channel.clone());
    }
}

/// Represents a channel between two nodes in the network graph.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct Channel {
    pub id: String,
    pub node1: String,
    pub node2: String,
    pub capacity: u64,
    pub delay: u64,
    pub base_fee_millisatoshi: u64,
    pub fee_per_millionth: u64,
}

impl Channel {
    /// Creates a new channel
    pub fn new(
        id: &str,
        node1: &str,
        node2: &str,
        capacity: u64,
        delay: u64,
        base_fee_millisatoshi: u64,
        fee_per_millionth: u64,
    ) -> Self {
        Channel {
            id: id.to_string(),
            node1: node1.to_string(),
            node2: node2.to_string(),
            capacity,
            delay,
            base_fee_millisatoshi,
            fee_per_millionth,
        }
    }

    /// Sets the capacity of the channel.
    pub fn set_capacity(&mut self, capacity: u64) {
        self.capacity = capacity;
    }
}

/// Trait for handling network graphs with channels, nodes, and peer-to-peer
/// information.
pub trait NetworkGraph {
    /// Gets all channels in the network graph.
    fn get_channels(&self) -> Vec<&Channel>;

    /// Gets all nodes in the network graph.
    fn get_nodes(&self) -> Vec<&Node>;

    /// Gets a node by its ID.
    fn get_node(&self, id: &str) -> Option<&Node>;

    /// Gets a channel by its ID.
    fn get_channel(&self, id: &str) -> Option<&Channel>;

    /// Whether or not the network graph has peer-to-peer information (e.g.,
    ///  gossip map).
    fn has_p2p_info(&self) -> bool;
}
