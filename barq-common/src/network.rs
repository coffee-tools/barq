//! The shared LDK network graph and scorer.
//!
//! The graph is built once per plugin lifetime and refreshed in the
//! background from two sources:
//!
//! - LDK rapid gossip sync snapshots (mainnet/testnet by default, or any
//!   self-hosted `rapid-gossip-sync-server`), using incremental snapshots based
//!   on the last sync timestamp stored inside the graph;
//! - the local Core Lightning `gossip_store`, read incrementally.
//!
//! Both the graph and the probabilistic scorer are persisted on disk so a
//! restart does not start from an empty view of the network.
use std::fs;
use std::io::{BufReader, Cursor, Write};
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use lampo_common::bitcoin::hashes::{sha256, Hash};
use lampo_common::conf::Network;
use lampo_common::ldk::ln::msgs::ChannelUpdate;
use lampo_common::ldk::routing::gossip::NetworkGraph as LdkNetworkGraph;
use lampo_common::ldk::routing::router::Path;
use lampo_common::ldk::routing::scoring::{
    ProbabilisticScorer, ProbabilisticScoringDecayParameters, ScoreUpdate,
};
use lampo_common::ldk::util::ser::{ReadableArgs, Writeable};
use lampo_common::utils::logger::LampoLogger;
use lightning_rapid_gossip_sync::RapidGossipSync;

use crate::gossip_store::{self, GossipStoreReader, GossipStoreStats};

pub type Logger = Arc<LampoLogger>;
pub type Graph = LdkNetworkGraph<Logger>;
pub type Scorer = ProbabilisticScorer<Arc<Graph>, Logger>;

/// Public LDK rapid gossip sync servers.
pub const LDK_RGS_MAINNET: &str = "https://rapidsync.lightningdevkit.org/snapshot";
pub const LDK_RGS_TESTNET: &str = "https://rapidsync.lightningdevkit.org/testnet/snapshot";

/// The default rapid gossip sync endpoint for a network, if any.
pub fn default_rgs_url(network: Network) -> Option<&'static str> {
    match network {
        Network::Bitcoin => Some(LDK_RGS_MAINNET),
        Network::Testnet => Some(LDK_RGS_TESTNET),
        _ => None,
    }
}

/// Outcome of a full refresh.
#[derive(Debug, Default)]
pub struct SyncReport {
    pub rapid_gossip_sync: Option<Result<u32>>,
    pub gossip_store: Option<Result<GossipStoreStats>>,
}

pub struct NetworkContext {
    pub network: Network,
    pub logger: Logger,
    pub graph: Arc<Graph>,
    pub scorer: Mutex<Scorer>,
    data_dir: PathBuf,
    gossip_store: Mutex<Option<GossipStoreReader>>,
    rgs_url: Option<String>,
    sync_lock: tokio::sync::Mutex<()>,
}

impl NetworkContext {
    /// Create (or reload from `data_dir`) the network context.
    ///
    /// `rgs_url` is the snapshot endpoint without the trailing timestamp,
    /// e.g. `https://rapidsync.lightningdevkit.org/snapshot`.
    pub fn new(
        network: Network,
        data_dir: PathBuf,
        gossip_store_path: Option<PathBuf>,
        rgs_url: Option<String>,
    ) -> Result<Arc<Self>> {
        fs::create_dir_all(&data_dir)
            .with_context(|| format!("creating {}", data_dir.display()))?;
        let logger = Arc::new(LampoLogger::new());

        let graph_path = Self::graph_path(&data_dir, network);
        let graph = match Self::load_graph(&graph_path, logger.clone()) {
            Some(graph) => {
                log::info!(
                    "loaded network graph from {} (last rgs timestamp {:?})",
                    graph_path.display(),
                    graph.get_last_rapid_gossip_sync_timestamp()
                );
                graph
            }
            None => LdkNetworkGraph::new(network, logger.clone()),
        };
        let graph = Arc::new(graph);

        let scorer_path = Self::scorer_path(&data_dir, network);
        let decay = ProbabilisticScoringDecayParameters::default();
        let scorer = Self::load_scorer(&scorer_path, decay, graph.clone(), logger.clone())
            .unwrap_or_else(|| ProbabilisticScorer::new(decay, graph.clone(), logger.clone()));

        Ok(Arc::new(Self {
            network,
            logger,
            graph,
            scorer: Mutex::new(scorer),
            data_dir,
            gossip_store: Mutex::new(gossip_store_path.map(GossipStoreReader::new)),
            rgs_url,
            sync_lock: tokio::sync::Mutex::new(()),
        }))
    }

    fn graph_path(data_dir: &FsPath, network: Network) -> PathBuf {
        data_dir.join(format!("network_graph.{network}.bin"))
    }

    fn scorer_path(data_dir: &FsPath, network: Network) -> PathBuf {
        data_dir.join(format!("scorer.{network}.bin"))
    }

    fn load_graph(path: &FsPath, logger: Logger) -> Option<Graph> {
        let file = fs::File::open(path).ok()?;
        let mut reader = BufReader::new(file);
        match Graph::read(&mut reader, logger) {
            Ok(graph) => Some(graph),
            Err(err) => {
                log::warn!(
                    "ignoring corrupted network graph {}: {err:?}",
                    path.display()
                );
                None
            }
        }
    }

    fn load_scorer(
        path: &FsPath,
        decay: ProbabilisticScoringDecayParameters,
        graph: Arc<Graph>,
        logger: Logger,
    ) -> Option<Scorer> {
        let bytes = fs::read(path).ok()?;
        let mut cursor = Cursor::new(bytes);
        match Scorer::read(&mut cursor, (decay, graph, logger)) {
            Ok(scorer) => Some(scorer),
            Err(err) => {
                log::warn!("ignoring corrupted scorer {}: {err:?}", path.display());
                None
            }
        }
    }

    pub fn rgs_url(&self) -> Option<&str> {
        self.rgs_url.as_deref()
    }

    pub fn gossip_store_path(&self) -> Option<PathBuf> {
        self.gossip_store
            .lock()
            .unwrap()
            .as_ref()
            .map(|r| r.path().to_path_buf())
    }

    /// The snapshot URL for the next incremental sync.
    fn rgs_snapshot_url(&self) -> Result<String> {
        let Some(base) = self.rgs_url.as_deref() else {
            anyhow::bail!("Rapid gossip sync is not configured for {}", self.network);
        };
        let last = self
            .graph
            .get_last_rapid_gossip_sync_timestamp()
            .unwrap_or(0);
        Ok(format!("{}/{}", base.trim_end_matches('/'), last))
    }

    /// Download the rapid gossip sync snapshot since the last known
    /// timestamp.
    pub async fn fetch_rapid_gossip_snapshot(&self) -> Result<Vec<u8>> {
        let url = self.rgs_snapshot_url()?;
        log::info!("rapid gossip sync: fetching {url}");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        let response = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("fetching rapid gossip sync snapshot from {url}"))?
            .error_for_status()
            .with_context(|| format!("rapid gossip sync server rejected {url}"))?;
        Ok(response.bytes().await?.to_vec())
    }

    /// Apply a rapid gossip sync snapshot to the graph. CPU bound: run it
    /// off the async executor. Returns the new sync timestamp.
    pub fn apply_rapid_gossip_snapshot(&self, snapshot: &[u8]) -> Result<u32> {
        let rapid_sync = RapidGossipSync::new(self.graph.clone(), self.logger.clone());
        let timestamp = rapid_sync
            .update_network_graph(snapshot)
            .map_err(|e| anyhow::anyhow!("Failed to apply rapid gossip sync snapshot: {e:?}"))?;
        log::info!(
            "rapid gossip sync: applied {} bytes, new timestamp {timestamp}",
            snapshot.len()
        );
        Ok(timestamp)
    }

    /// Fetch and apply the next rapid gossip sync snapshot.
    pub async fn sync_rapid_gossip(self: &Arc<Self>) -> Result<u32> {
        let snapshot = self.fetch_rapid_gossip_snapshot().await?;
        let ctx = self.clone();
        tokio::task::spawn_blocking(move || ctx.apply_rapid_gossip_snapshot(&snapshot)).await?
    }

    /// Feed any new record of the Core Lightning gossip store to the graph.
    ///
    /// Blocking file IO: call it from `spawn_blocking` in async code, or use
    /// [`Self::sync_gossip_store_async`].
    pub fn sync_gossip_store(&self) -> Result<GossipStoreStats> {
        let mut guard = self.gossip_store.lock().unwrap();
        let Some(reader) = guard.as_mut() else {
            anyhow::bail!("No gossip store configured");
        };
        gossip_store::sync(reader, &self.graph)
    }

    pub async fn sync_gossip_store_async(self: &Arc<Self>) -> Result<GossipStoreStats> {
        let ctx = self.clone();
        tokio::task::spawn_blocking(move || ctx.sync_gossip_store()).await?
    }

    /// Refresh the graph from every configured source and persist it.
    ///
    /// The rapid gossip sync download and the gossip_store read run
    /// concurrently.
    pub async fn sync_all(self: &Arc<Self>) -> SyncReport {
        let _guard = self.sync_lock.lock().await;
        let rgs = async {
            if self.rgs_url.is_some() {
                Some(self.sync_rapid_gossip().await)
            } else {
                None
            }
        };
        let store = async {
            if self.gossip_store.lock().unwrap().is_some() {
                Some(self.sync_gossip_store_async().await)
            } else {
                None
            }
        };
        let (rapid_gossip_sync, gossip_store) = tokio::join!(rgs, store);
        let ctx = self.clone();
        let persisted = tokio::task::spawn_blocking(move || {
            ctx.graph.remove_stale_channels_and_tracking();
            ctx.persist()
        })
        .await;
        match persisted {
            Ok(Err(err)) => log::warn!("failed to persist network graph: {err}"),
            Err(err) => log::warn!("graph persistence task failed: {err}"),
            Ok(Ok(())) => {}
        }
        SyncReport {
            rapid_gossip_sync,
            gossip_store,
        }
    }

    /// Spawn a task on `runtime` that refreshes the graph every `interval`.
    pub fn start_background_sync(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        interval: Duration,
    ) {
        let ctx = self.clone();
        runtime.spawn(async move {
            loop {
                let report = ctx.sync_all().await;
                if let Some(Err(err)) = &report.rapid_gossip_sync {
                    log::warn!("rapid gossip sync failed: {err}");
                }
                if let Some(Err(err)) = &report.gossip_store {
                    log::warn!("gossip store sync failed: {err}");
                }
                if let Some(Ok(stats)) = &report.gossip_store {
                    log::debug!("gossip store sync: {stats:?}");
                }
                tokio::time::sleep(interval).await;
            }
        });
    }

    /// Write the graph and the scorer to disk.
    pub fn persist(&self) -> Result<()> {
        let graph_path = Self::graph_path(&self.data_dir, self.network);
        Self::atomic_write(&graph_path, &self.graph.encode())?;
        let scorer_bytes = self.scorer.lock().unwrap().encode();
        let scorer_path = Self::scorer_path(&self.data_dir, self.network);
        Self::atomic_write(&scorer_path, &scorer_bytes)?;
        Ok(())
    }

    fn atomic_write(path: &FsPath, bytes: &[u8]) -> Result<()> {
        let tmp = path.with_extension("tmp");
        {
            let mut file =
                fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
        Ok(())
    }

    fn now() -> Duration {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
    }

    /// Tell the scorer the payment along `path` succeeded.
    pub fn payment_succeeded(&self, path: &Path) {
        self.scorer
            .lock()
            .unwrap()
            .payment_path_successful(path, Self::now());
        if let Err(err) = self.persist() {
            log::warn!("failed to persist scorer: {err}");
        }
    }

    /// Apply a `channel_update` learned from an onion failure to the graph,
    /// so the next route uses the policy the node really enforces.
    ///
    /// The signature is verified against the node keys of the announced
    /// channel: a forwarding node cannot plant a policy for a channel that
    /// is not its own. Updates for channels the graph does not know, or
    /// older than the one it has, are rejected.
    pub fn apply_channel_update(&self, update: &ChannelUpdate) -> Result<()> {
        self.graph
            .update_channel(update)
            .map_err(|err| anyhow::anyhow!("{}", err.err))?;
        if let Err(err) = self.persist() {
            log::warn!("failed to persist network graph: {err}");
        }
        Ok(())
    }

    /// Tell the scorer the payment along `path` failed at channel `scid`.
    pub fn payment_failed(&self, path: &Path, scid: u64) {
        self.scorer
            .lock()
            .unwrap()
            .payment_path_failed(path, scid, Self::now());
        if let Err(err) = self.persist() {
            log::warn!("failed to persist scorer: {err}");
        }
    }

    /// 32 random-looking bytes for the router, derived from the clock and
    /// the process id. Routing only uses them to add jitter.
    pub fn random_seed(&self) -> [u8; 32] {
        let mut data = Vec::with_capacity(24);
        data.extend_from_slice(&Self::now().as_nanos().to_le_bytes());
        data.extend_from_slice(&std::process::id().to_le_bytes());
        data.extend_from_slice(&(self as *const Self as usize).to_le_bytes());
        sha256::Hash::hash(&data).to_byte_array()
    }

    /// The channels of `node` in the graph, to route around the node.
    pub fn channels_of(&self, node: &lampo_common::secp256k1::PublicKey) -> Vec<u64> {
        let graph = self.graph.read_only();
        graph
            .node(&lampo_common::ldk::routing::gossip::NodeId::from_pubkey(
                node,
            ))
            .map(|info| info.channels.clone())
            .unwrap_or_default()
    }

    /// Number of channels and nodes currently known.
    pub fn size(&self) -> (usize, usize) {
        let graph = self.graph.read_only();
        (graph.channels().len(), graph.nodes().len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hits the public LDK server: run with `cargo test -- --ignored`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn rapid_gossip_sync_mainnet_live() {
        let dir = std::env::temp_dir().join(format!("barq-rgs-live-{}", std::process::id()));
        let ctx = NetworkContext::new(
            Network::Bitcoin,
            dir.clone(),
            None,
            Some(LDK_RGS_MAINNET.to_owned()),
        )
        .unwrap();
        let first = ctx.sync_rapid_gossip().await.unwrap();
        let (channels, nodes) = ctx.size();
        assert!(channels > 10_000, "only {channels} channels");
        assert!(nodes > 1_000, "only {nodes} nodes");

        // Persist, reload, and continue incrementally from the same timestamp.
        ctx.persist().unwrap();
        drop(ctx);
        let ctx = NetworkContext::new(
            Network::Bitcoin,
            dir.clone(),
            None,
            Some(LDK_RGS_MAINNET.to_owned()),
        )
        .unwrap();
        assert_eq!(
            ctx.graph.get_last_rapid_gossip_sync_timestamp(),
            Some(first)
        );
        assert!(ctx
            .rgs_snapshot_url()
            .unwrap()
            .ends_with(&format!("/{first}")));
        let second = ctx.sync_rapid_gossip().await.unwrap();
        assert!(second >= first);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
