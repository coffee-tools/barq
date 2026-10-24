//! Simulated network graphs for testing and benchmarking routing strategies.
//!
//! Real Lightning networks are slow to build in integration tests (one
//! bitcoind and one lightningd per node), which makes it impossible to test
//! a strategy on more than a handful of nodes. [`SimulatedNetwork`] builds
//! an LDK network graph directly: nodes with deterministic keys, channels
//! with a fee policy per direction, and a few topology generators (line,
//! grid, random). The same graph can be described in JSON with
//! [`GraphSpec`], so the plugin can load a mocked graph next to its real one
//! during integration tests (`barq-dev-simulated-graph`).
//!
//! See issue #9.
use std::path::Path as FsPath;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use lampo_common::bitcoin::hashes::{sha256, Hash};
use lampo_common::conf::Network;
use lampo_common::ldk::ln::features::ChannelFeatures;
use lampo_common::ldk::ln::msgs::UnsignedChannelUpdate;
use lampo_common::secp256k1::{PublicKey, Secp256k1, SecretKey};

use crate::network::{Graph, NetworkContext};
use crate::scid;
use crate::strategy::{FirstHop, Payee, RouteInput};

/// The routing policy of one direction of a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default = "default_base")]
    pub fee_base_msat: u32,
    #[serde(default = "default_ppm")]
    pub fee_proportional_millionths: u32,
    #[serde(default = "default_cltv")]
    pub cltv_expiry_delta: u16,
    #[serde(default = "default_htlc_min")]
    pub htlc_minimum_msat: u64,
    /// Defaults to the channel capacity.
    #[serde(default)]
    pub htlc_maximum_msat: Option<u64>,
    #[serde(default)]
    pub disabled: bool,
}

fn default_base() -> u32 {
    1000
}
fn default_ppm() -> u32 {
    10
}
fn default_cltv() -> u16 {
    6
}
fn default_htlc_min() -> u64 {
    1
}

impl Default for Policy {
    /// Core Lightning's defaults: 1 sat base fee, 10 ppm, 6 blocks.
    fn default() -> Self {
        Self {
            fee_base_msat: default_base(),
            fee_proportional_millionths: default_ppm(),
            cltv_expiry_delta: default_cltv(),
            htlc_minimum_msat: default_htlc_min(),
            htlc_maximum_msat: None,
            disabled: false,
        }
    }
}

impl Policy {
    pub fn with_fees(fee_base_msat: u32, fee_proportional_millionths: u32) -> Self {
        Self {
            fee_base_msat,
            fee_proportional_millionths,
            ..Self::default()
        }
    }

    /// Fee charged to forward `amount_msat` over this direction.
    pub fn fee_msat(&self, amount_msat: u64) -> u64 {
        self.fee_base_msat as u64
            + amount_msat * self.fee_proportional_millionths as u64 / 1_000_000
    }
}

/// One channel of a [`GraphSpec`]. Nodes are hex public keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSpec {
    /// `BLOCKxTXxOUT`; generated when missing.
    #[serde(default)]
    pub short_channel_id: Option<String>,
    pub node1: String,
    pub node2: String,
    pub capacity_sat: u64,
    /// Policy of `node1 -> node2`.
    #[serde(default)]
    pub policy_1to2: Policy,
    /// Policy of `node2 -> node1`.
    #[serde(default)]
    pub policy_2to1: Policy,
}

/// A JSON-serializable description of a mocked graph.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphSpec {
    pub channels: Vec<ChannelSpec>,
}

impl GraphSpec {
    pub fn from_file<P: AsRef<FsPath>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading graph spec {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing graph spec {}", path.display()))
    }

    /// Add every channel to `graph`. Returns the number of channels added.
    pub fn apply(&self, graph: &Graph) -> Result<usize> {
        let mut next_scid = SCID_BASE;
        for channel in &self.channels {
            let a = PublicKey::from_str(&channel.node1)
                .with_context(|| format!("invalid node1 `{}`", channel.node1))?;
            let b = PublicKey::from_str(&channel.node2)
                .with_context(|| format!("invalid node2 `{}`", channel.node2))?;
            let short_channel_id = match &channel.short_channel_id {
                Some(s) => scid::from_cln(s)?,
                None => {
                    next_scid += 1 << 16;
                    next_scid
                }
            };
            insert_channel(
                graph,
                short_channel_id,
                a,
                b,
                channel.capacity_sat,
                &channel.policy_1to2,
                &channel.policy_2to1,
            )?;
        }
        Ok(self.channels.len())
    }
}

/// Simulated channels start at block 500_000 so they never collide with
/// regtest channels.
const SCID_BASE: u64 = 500_000 << 40;

fn now() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}

/// Add a channel between `a` and `b` with a policy per direction.
pub fn insert_channel(
    graph: &Graph,
    short_channel_id: u64,
    a: PublicKey,
    b: PublicKey,
    capacity_sat: u64,
    policy_ab: &Policy,
    policy_ba: &Policy,
) -> Result<()> {
    // BOLT 7: node_id_1 is the lexicographically smaller key; direction 0
    // of a channel_update is node_id_1 -> node_id_2.
    let (one, two, policy_12, policy_21) = if a.serialize() < b.serialize() {
        (a, b, policy_ab, policy_ba)
    } else {
        (b, a, policy_ba, policy_ab)
    };
    let timestamp = now();
    graph
        .add_channel_from_partial_announcement(
            short_channel_id,
            timestamp as u64,
            ChannelFeatures::empty(),
            one,
            two,
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "adding channel {}: {}",
                scid::to_cln(short_channel_id),
                e.err
            )
        })?;
    for (direction, policy) in [(0u8, policy_12), (1u8, policy_21)] {
        let flags = direction | if policy.disabled { 0b10 } else { 0 };
        let update = UnsignedChannelUpdate {
            chain_hash: graph.get_chain_hash(),
            short_channel_id,
            timestamp,
            flags,
            cltv_expiry_delta: policy.cltv_expiry_delta,
            htlc_minimum_msat: policy.htlc_minimum_msat,
            htlc_maximum_msat: policy.htlc_maximum_msat.unwrap_or(capacity_sat * 1000),
            fee_base_msat: policy.fee_base_msat,
            fee_proportional_millionths: policy.fee_proportional_millionths,
            excess_data: vec![],
        };
        graph.update_channel_unsigned(&update).map_err(|e| {
            anyhow::anyhow!(
                "updating channel {}: {}",
                scid::to_cln(short_channel_id),
                e.err
            )
        })?;
    }
    Ok(())
}

/// A channel of a [`SimulatedNetwork`], by node index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimChannel {
    pub scid: u64,
    pub a: usize,
    pub b: usize,
    pub capacity_sat: u64,
    pub policy_ab: Policy,
    pub policy_ba: Policy,
}

/// A graph of simulated nodes and channels, held in a [`NetworkContext`] so
/// the real strategies can route on it.
pub struct SimulatedNetwork {
    pub ctx: Arc<NetworkContext>,
    keys: Vec<PublicKey>,
    channels: Vec<SimChannel>,
    next_scid: u64,
}

impl SimulatedNetwork {
    /// An empty regtest graph, stored under a fresh temporary directory.
    pub fn new() -> Result<Self> {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "barq-sim-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let ctx = NetworkContext::new(Network::Regtest, dir, None, None)?;
        Ok(Self {
            ctx,
            keys: vec![],
            channels: vec![],
            next_scid: SCID_BASE,
        })
    }

    /// The deterministic public key of node `index`.
    pub fn key_for(index: usize) -> PublicKey {
        let seed = sha256::Hash::hash(format!("barq-simulated-node-{index}").as_bytes());
        let secret = SecretKey::from_slice(seed.as_byte_array()).expect("valid secret key");
        PublicKey::from_secret_key(&Secp256k1::new(), &secret)
    }

    /// Add a node, returning its index.
    pub fn add_node(&mut self) -> usize {
        let index = self.keys.len();
        self.keys.push(Self::key_for(index));
        index
    }

    pub fn add_nodes(&mut self, count: usize) -> Vec<usize> {
        (0..count).map(|_| self.add_node()).collect()
    }

    pub fn node_id(&self, index: usize) -> PublicKey {
        self.keys[index]
    }

    pub fn node_count(&self) -> usize {
        self.keys.len()
    }

    pub fn channels(&self) -> &[SimChannel] {
        &self.channels
    }

    /// Open a channel between nodes `a` and `b`, returning its scid.
    pub fn add_channel(
        &mut self,
        a: usize,
        b: usize,
        capacity_sat: u64,
        policy_ab: Policy,
        policy_ba: Policy,
    ) -> Result<u64> {
        self.next_scid += 1 << 16;
        let scid = self.next_scid;
        insert_channel(
            &self.ctx.graph,
            scid,
            self.keys[a],
            self.keys[b],
            capacity_sat,
            &policy_ab,
            &policy_ba,
        )?;
        self.channels.push(SimChannel {
            scid,
            a,
            b,
            capacity_sat,
            policy_ab,
            policy_ba,
        });
        Ok(scid)
    }

    /// `n` nodes in a line: 0 - 1 - ... - n-1.
    pub fn line(n: usize, capacity_sat: u64, policy: Policy) -> Result<Self> {
        let mut net = Self::new()?;
        let nodes = net.add_nodes(n);
        for pair in nodes.windows(2) {
            net.add_channel(pair[0], pair[1], capacity_sat, policy, policy)?;
        }
        Ok(net)
    }

    /// A `width x height` grid; node `(x, y)` has index `y * width + x`.
    pub fn grid(width: usize, height: usize, capacity_sat: u64, policy: Policy) -> Result<Self> {
        let mut net = Self::new()?;
        net.add_nodes(width * height);
        for y in 0..height {
            for x in 0..width {
                let here = y * width + x;
                if x + 1 < width {
                    net.add_channel(here, here + 1, capacity_sat, policy, policy)?;
                }
                if y + 1 < height {
                    net.add_channel(here, here + width, capacity_sat, policy, policy)?;
                }
            }
        }
        Ok(net)
    }

    /// A connected random graph: a random spanning tree plus `extra`
    /// random channels, with random capacities and fees. Deterministic for
    /// a given `seed`.
    pub fn random(nodes: usize, extra: usize, seed: u64) -> Result<Self> {
        let mut rng = XorShift(seed.max(1));
        let mut net = Self::new()?;
        net.add_nodes(nodes);
        let random_policy = |rng: &mut XorShift| Policy {
            fee_base_msat: rng.below(2000) as u32,
            fee_proportional_millionths: rng.below(1000) as u32,
            cltv_expiry_delta: 6 + rng.below(140) as u16,
            ..Policy::default()
        };
        for node in 1..nodes {
            let peer = rng.below(node as u64) as usize;
            let capacity = 100_000 + rng.below(10_000_000);
            let (ab, ba) = (random_policy(&mut rng), random_policy(&mut rng));
            net.add_channel(node, peer, capacity, ab, ba)?;
        }
        let mut added = 0;
        while added < extra && nodes > 2 {
            let a = rng.below(nodes as u64) as usize;
            let b = rng.below(nodes as u64) as usize;
            if a == b {
                continue;
            }
            let capacity = 100_000 + rng.below(10_000_000);
            let (ab, ba) = (random_policy(&mut rng), random_policy(&mut rng));
            net.add_channel(a, b, capacity, ab, ba)?;
            added += 1;
        }
        Ok(net)
    }

    /// The channels of `node` as first hops, as if `node` were our node
    /// and every channel were fully spendable from its side.
    pub fn first_hops(&self, node: usize) -> Vec<FirstHop> {
        self.channels
            .iter()
            .filter(|c| c.a == node || c.b == node)
            .map(|c| {
                let peer = if c.a == node { c.b } else { c.a };
                FirstHop {
                    peer_id: self.keys[peer],
                    scid: c.scid,
                    cln_channel: scid::to_cln(c.scid),
                    capacity_sat: c.capacity_sat,
                    spendable_msat: c.capacity_sat * 1000,
                    htlc_minimum_msat: 1,
                    htlc_maximum_msat: c.capacity_sat * 1000,
                    is_public: true,
                }
            })
            .collect()
    }

    /// A [`RouteInput`] paying `amount_msat` from `from` to `to`.
    pub fn route_input(&self, from: usize, to: usize, amount_msat: u64) -> RouteInput {
        RouteInput {
            src_pubkey: self.keys[from],
            payee: Payee::Clear {
                node_id: self.keys[to],
                route_hints: vec![],
                features: None,
            },
            amount_msat,
            final_cltv_delta: 18,
            first_hops: self.first_hops(from),
            expiry_time: None,
            strategy: None,
            excluded_channels: vec![],
        }
    }

    /// Describe this network as a [`GraphSpec`].
    pub fn to_spec(&self) -> GraphSpec {
        GraphSpec {
            channels: self
                .channels
                .iter()
                .map(|c| ChannelSpec {
                    short_channel_id: Some(scid::to_cln(c.scid)),
                    node1: self.keys[c.a].to_string(),
                    node2: self.keys[c.b].to_string(),
                    capacity_sat: c.capacity_sat,
                    policy_1to2: c.policy_ab,
                    policy_2to1: c.policy_ba,
                })
                .collect(),
        }
    }
}

/// Tiny deterministic PRNG, so the generators need no extra dependency.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            0
        } else {
            self.next() % bound
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithms::direct::Direct;
    use crate::algorithms::probabilistic::LDKRoutingStrategy;
    use crate::strategy::Strategy;

    fn ldk(net: &SimulatedNetwork) -> LDKRoutingStrategy {
        LDKRoutingStrategy::new(net.ctx.clone())
    }

    #[test]
    fn keys_are_deterministic_and_distinct() {
        assert_eq!(SimulatedNetwork::key_for(3), SimulatedNetwork::key_for(3));
        assert_ne!(SimulatedNetwork::key_for(3), SimulatedNetwork::key_for(4));
    }

    #[test]
    fn line_routes_through_every_hop_and_charges_each_forwarder() {
        let policy = Policy::with_fees(1000, 10);
        let net = SimulatedNetwork::line(5, 1_000_000, policy).unwrap();
        assert_eq!(net.ctx.size(), (4, 5));

        let amount = 100_000;
        let route = ldk(&net).route(&net.route_input(0, 4, amount)).unwrap();
        let ids: Vec<_> = route.path.hops.iter().map(|h| h.pubkey).collect();
        assert_eq!(ids, (1..5).map(|i| net.node_id(i)).collect::<Vec<_>>());
        // Three forwarders (1, 2, 3). Each fee is computed on the amount it
        // forwards, so it is at least the fee on the bare amount.
        assert!(route.fee_msat() >= 3 * policy.fee_msat(amount));
        assert!(route.fee_msat() < 3 * policy.fee_msat(amount) + 10);
    }

    #[test]
    fn picks_the_cheaper_of_two_paths() {
        // 0 -> 1 -> 3 is expensive, 0 -> 2 -> 3 is cheap.
        let mut net = SimulatedNetwork::new().unwrap();
        net.add_nodes(4);
        let cheap = Policy::with_fees(0, 1);
        let pricey = Policy::with_fees(50_000, 5_000);
        net.add_channel(0, 1, 1_000_000, cheap, cheap).unwrap();
        net.add_channel(1, 3, 1_000_000, pricey, pricey).unwrap();
        net.add_channel(0, 2, 1_000_000, cheap, cheap).unwrap();
        net.add_channel(2, 3, 1_000_000, cheap, cheap).unwrap();

        let route = ldk(&net).route(&net.route_input(0, 3, 50_000)).unwrap();
        assert_eq!(route.path.hops[0].pubkey, net.node_id(2));
    }

    #[test]
    fn disabled_and_too_small_channels_are_avoided() {
        let mut net = SimulatedNetwork::new().unwrap();
        net.add_nodes(4);
        let p = Policy::default();
        let disabled = Policy {
            disabled: true,
            ..p
        };
        let tiny = Policy {
            htlc_maximum_msat: Some(10_000),
            ..p
        };
        net.add_channel(0, 1, 1_000_000, p, p).unwrap();
        net.add_channel(1, 3, 1_000_000, disabled, p).unwrap(); // 1 -> 3 disabled
        net.add_channel(0, 2, 1_000_000, p, p).unwrap();
        net.add_channel(2, 3, 1_000_000, tiny, p).unwrap(); // 2 -> 3 max 10 sat

        // 5 sat fits the tiny channel: only 0 -> 2 -> 3 works.
        let route = ldk(&net).route(&net.route_input(0, 3, 5_000)).unwrap();
        assert_eq!(route.path.hops[0].pubkey, net.node_id(2));
        // 50 sat fits neither path.
        assert!(ldk(&net).route(&net.route_input(0, 3, 50_000)).is_err());
    }

    #[test]
    fn unreachable_node_fails() {
        let mut net = SimulatedNetwork::line(3, 1_000_000, Policy::default()).unwrap();
        let island = net.add_node();
        assert!(ldk(&net).route(&net.route_input(0, island, 1_000)).is_err());
    }

    #[test]
    fn grid_routes_with_the_minimum_number_of_hops() {
        let net = SimulatedNetwork::grid(5, 5, 1_000_000, Policy::default()).unwrap();
        assert_eq!(net.ctx.size(), (40, 25));
        // From corner (0,0) to corner (4,4): 8 hops on a grid with equal fees.
        let route = ldk(&net).route(&net.route_input(0, 24, 10_000)).unwrap();
        assert_eq!(route.path.hops.len(), 8);
    }

    #[test]
    fn direct_strategy_only_handles_peers() {
        let net = SimulatedNetwork::line(3, 1_000_000, Policy::default()).unwrap();
        let direct = Direct::new();
        assert!(direct.can_apply(&net.route_input(0, 1, 1_000)).unwrap());
        assert!(!direct.can_apply(&net.route_input(0, 2, 1_000)).unwrap());
    }

    #[test]
    fn random_graph_is_deterministic_and_routable() {
        let a = SimulatedNetwork::random(200, 400, 7).unwrap();
        let b = SimulatedNetwork::random(200, 400, 7).unwrap();
        assert_eq!(a.channels(), b.channels());
        assert_eq!(a.ctx.size().1, 200);
        // The spanning tree makes every node reachable for a small amount.
        let strategy = ldk(&a);
        let routed = (1..50)
            .filter(|&to| strategy.route(&a.route_input(0, to, 1_000)).is_ok())
            .count();
        assert_eq!(routed, 49);
    }

    #[test]
    fn spec_roundtrip_rebuilds_the_same_graph() {
        let net = SimulatedNetwork::grid(3, 3, 500_000, Policy::with_fees(5, 50)).unwrap();
        let spec = net.to_spec();
        let json = serde_json::to_string(&spec).unwrap();
        let parsed: GraphSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, spec);

        let other = SimulatedNetwork::new().unwrap();
        assert_eq!(parsed.apply(&other.ctx.graph).unwrap(), 12);
        assert_eq!(other.ctx.size(), net.ctx.size());
    }

    #[test]
    fn spec_defaults_and_errors() {
        let a = SimulatedNetwork::key_for(1).to_string();
        let b = SimulatedNetwork::key_for(2).to_string();
        let json =
            format!(r#"{{"channels":[{{"node1":"{a}","node2":"{b}","capacity_sat":1000}}]}}"#);
        let spec: GraphSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec.channels[0].policy_1to2, Policy::default());
        let net = SimulatedNetwork::new().unwrap();
        assert_eq!(spec.apply(&net.ctx.graph).unwrap(), 1);

        let bad = GraphSpec {
            channels: vec![ChannelSpec {
                short_channel_id: None,
                node1: "not-a-key".to_owned(),
                node2: b,
                capacity_sat: 1,
                policy_1to2: Policy::default(),
                policy_2to1: Policy::default(),
            }],
        };
        assert!(bad.apply(&net.ctx.graph).is_err());
    }
}
