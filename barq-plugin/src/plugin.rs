//! Barq Plugin implementation

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use clightningrpc_common::errors::RpcError;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::runtime::Runtime;

use clightningrpc::LightningRPC;
use clightningrpc_plugin::commands::RPCCommand;
use clightningrpc_plugin::errors::PluginError;
use clightningrpc_plugin::plugin::Plugin;
use clightningrpc_plugin_macros::{plugin, rpc_method};

use barq_common::algorithms::external::ExternalStrategy;
use barq_common::history::PaymentStore;
use barq_common::network::{default_rgs_url, NetworkContext};
use barq_common::simulation::GraphSpec;
use barq_common::Network;

use crate::methods;
use crate::methods::pay_hook::{self, PayHook};

/// Plugin options.
pub const OPT_RGS_URL: &str = "barq-rgs-url";
pub const OPT_SYNC_INTERVAL: &str = "barq-sync-interval";
pub const OPT_CLN_GOSSIP: &str = "barq-cln-gossip";
pub const OPT_MAX_ATTEMPTS: &str = "barq-max-attempts";
pub const OPT_OVERRIDE_PAY: &str = "barq-override-pay";
pub const OPT_EXTERNAL_STRATEGY: &str = "barq-external-strategy";
pub const OPT_EXTERNAL_TIMEOUT: &str = "barq-external-strategy-timeout";
/// Developer option: load a mocked graph (JSON `GraphSpec`) at startup.
pub const OPT_DEV_SIMULATED_GRAPH: &str = "barq-dev-simulated-graph";

/// Barq Plugin State
///
/// This struct holds the CLN RPC path and the shared LDK network graph.
#[derive(Clone)]
pub(crate) struct State {
    /// CLN RPC path
    ///
    /// eg. /home/user/.lightning/lightning-rpc
    pub(crate) cln_rpc_path: Option<String>,
    pub(crate) network: Option<Network>,
    /// The LDK network graph, scorer and gossip sources.
    pub(crate) ctx: Option<Arc<NetworkContext>>,
    /// Runs the async RPC calls and the background gossip sync.
    pub(crate) runtime: Arc<Runtime>,
    /// Default number of routes `barqpay` tries.
    pub(crate) max_attempts: u32,
    /// The payment history.
    pub(crate) store: Option<Arc<PaymentStore>>,
    /// Serve Core Lightning's `pay` command (issue #36).
    pub(crate) override_pay: bool,
    /// Whether lightningd supports the filtered `rpc_command` hook.
    pub(crate) pay_hook_registered: bool,
    /// A strategy implemented by an external program (issue #12).
    pub(crate) external_strategy: Option<ExternalStrategy>,
}

impl State {
    /// Create a new Barq Plugin State
    pub fn new() -> Self {
        State {
            cln_rpc_path: None,
            network: None,
            ctx: None,
            runtime: Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("barq-rt")
                    .enable_all()
                    .build()
                    .expect("building the tokio runtime"),
            ),
            max_attempts: 3,
            store: None,
            override_pay: false,
            pay_hook_registered: false,
            external_strategy: None,
        }
    }

    /// Drive `future` to completion from a synchronous cln4rust handler.
    pub fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    pub fn store(&self) -> Result<Arc<PaymentStore>, PluginError> {
        self.store
            .clone()
            .ok_or_else(|| PluginError::new(-1, "barq is not initialized yet", None))
    }

    pub fn ctx(&self) -> Result<Arc<NetworkContext>, PluginError> {
        self.ctx
            .clone()
            .ok_or_else(|| PluginError::new(-1, "barq is not initialized yet", None))
    }

    /// Call a CLN RPC method with the cln4rust async client.
    pub async fn call<T: Serialize, U: DeserializeOwned + std::fmt::Debug>(
        &self,
        method: &str,
        payload: T,
    ) -> Result<U, RpcError> {
        let path = self.cln_rpc_path.as_ref().ok_or(RpcError {
            code: -1,
            message: "CLN RPC path is unset".to_owned(),
            data: None,
        })?;
        let rpc = LightningRPC::new(path);
        let response = rpc.call::<T, U>(method, payload).await;
        log::trace!("cln `{method}` response: {:?}", response);
        // We need to clean up the errors inside the library a bit
        match response {
            Ok(response) => Ok(response),
            Err(clightningrpc::errors::Error::Rpc(err)) => Err(RpcError {
                code: err.code,
                message: err.message,
                data: err.data,
            }),
            Err(err) => Err(RpcError {
                code: -1,
                message: format!("{err}"),
                data: None,
            }),
        }
    }
}

/// Build the Barq Plugin
pub fn build_plugin() -> anyhow::Result<Plugin<State>> {
    let mut plugin = plugin! {
        state: State::new(),
        dynamic: true,
        notification: [],
        methods: [
            barq_pay,
            barq_route_info,
            barq_sync_gossip,
            barq_inspect,
        ],
        hooks: [],
    };
    plugin
        .add_opt(
            OPT_RGS_URL,
            "string",
            Some("auto".to_owned()),
            "LDK rapid gossip sync snapshot endpoint (without the trailing timestamp). \
             `auto` picks the public LDK server on mainnet/testnet, `off` disables it.",
            false,
        )
        .add_opt(
            OPT_SYNC_INTERVAL,
            "int",
            Some("3600".to_owned()),
            "Seconds between background gossip refreshes (0 disables the background sync).",
            false,
        )
        .add_opt(
            OPT_CLN_GOSSIP,
            "bool",
            Some("true".to_owned()),
            "Feed the LDK graph with the Core Lightning gossip_store.",
            false,
        )
        .add_opt(
            OPT_MAX_ATTEMPTS,
            "int",
            Some("3".to_owned()),
            "How many routes barqpay tries before giving up; each failure excludes the \
             failing channel and re-ranks the strategies.",
            false,
        )
        .add_opt(
            OPT_OVERRIDE_PAY,
            "bool",
            Some("false".to_owned()),
            "Serve Core Lightning's `pay` command with barq (needs Core Lightning v25.12 or later).",
            false,
        )
        .add_opt(
            OPT_EXTERNAL_STRATEGY,
            "string",
            None,
            "Path to a program implementing barq's external strategy protocol; it is \
             ranked as the `external` strategy.",
            false,
        )
        .add_opt(
            OPT_EXTERNAL_TIMEOUT,
            "int",
            Some("30".to_owned()),
            "Seconds the external strategy program may take to answer.",
            false,
        )
        .add_opt(
            OPT_DEV_SIMULATED_GRAPH,
            "string",
            None,
            "Developer option: path to a JSON graph spec of mocked channels loaded into \
             the routing graph at startup, for testing strategies on large topologies.",
            false,
        );
    plugin.register_notification("shutdown", OnShutdown);
    // The hook must be declared in the manifest, before options are known:
    // register it whenever lightningd supports the `pay` filter, and let
    // `barq-override-pay` decide at runtime.
    let version = std::env::var("LIGHTNINGD_VERSION").ok();
    if pay_hook::supports_filters(version.as_deref()) {
        // rpc_command is a chained hook where the first plugin that changes
        // the request wins. Since v25.12 `cln-xpay` takes over `pay` the same
        // way, so barq must come first.
        plugin.register_hook_with_filters(
            "rpc_command",
            Some(vec!["cln-xpay".to_owned()]),
            None,
            Some(vec![serde_json::json!("pay")]),
            PayHook,
        );
        plugin.state.pay_hook_registered = true;
    }
    plugin.on_init(on_init);
    Ok(plugin)
}

/// This method is called when the plugin is initialized
fn on_init(plugin: &mut Plugin<State>) -> Value {
    let config = plugin.configuration.clone().unwrap();
    let lightning_dir = PathBuf::from(&config.lightning_dir);
    // Unix socket paths are limited to ~104 bytes (SUN_LEN), so a deep
    // lightning-dir makes the absolute `lightning-rpc` path unusable. Like
    // Core Lightning's own plugins, work from the lightning-dir and connect
    // with the relative socket name.
    let rpc_file = match std::env::set_current_dir(&lightning_dir) {
        Ok(()) => PathBuf::from(&config.rpc_file),
        Err(err) => {
            log::warn!(
                "cannot change directory to {}: {err}; using the absolute RPC path",
                lightning_dir.display()
            );
            lightning_dir.join(&config.rpc_file)
        }
    };
    plugin.state.cln_rpc_path = Some(rpc_file.to_string_lossy().to_string());

    let network = match Network::from_str(&config.network) {
        Ok(network) => network,
        Err(err) => {
            return serde_json::json!({ "disable": format!("unknown network `{}`: {err}", config.network) })
        }
    };
    plugin.state.network = Some(network);

    let rgs_opt: String = plugin
        .get_opt::<String>(OPT_RGS_URL)
        .unwrap_or_else(|| "auto".to_owned());
    let rgs_url = match rgs_opt.trim() {
        "" | "auto" => default_rgs_url(network).map(|u| u.to_owned()),
        "off" | "none" | "false" => None,
        url => Some(url.to_owned()),
    };
    let use_cln_gossip: bool = plugin.get_opt::<bool>(OPT_CLN_GOSSIP).unwrap_or(true);
    let interval: i64 = plugin.get_opt::<i64>(OPT_SYNC_INTERVAL).unwrap_or(3600);
    if let Some(program) = plugin.get_opt::<String>(OPT_EXTERNAL_STRATEGY) {
        let mut strategy = ExternalStrategy::new(&program);
        strategy.workdir = Some(lightning_dir.clone());
        strategy.rpc_file = config.rpc_file.clone();
        strategy.timeout = Duration::from_secs(
            plugin
                .get_opt::<i64>(OPT_EXTERNAL_TIMEOUT)
                .unwrap_or(30)
                .clamp(1, 600) as u64,
        );
        log::info!("external strategy: {program}");
        plugin.state.external_strategy = Some(strategy);
    }
    plugin.state.override_pay = plugin.get_opt::<bool>(OPT_OVERRIDE_PAY).unwrap_or(false);
    if plugin.state.override_pay {
        if plugin.state.pay_hook_registered {
            log::info!("barq serves the `pay` command");
        } else {
            log::warn!(
                "barq-override-pay needs Core Lightning v{}.{} or later (rpc_command filters); `pay` is not served by barq",
                pay_hook::FILTERS_SINCE.0,
                pay_hook::FILTERS_SINCE.1
            );
        }
    }
    plugin.state.max_attempts = plugin
        .get_opt::<i64>(OPT_MAX_ATTEMPTS)
        .unwrap_or(3)
        .clamp(1, 100) as u32;

    let gossip_store = if use_cln_gossip {
        Some(lightning_dir.join("gossip_store"))
    } else {
        None
    };
    let data_dir = lightning_dir.join("barq");
    match PaymentStore::open(data_dir.join("payments")) {
        Ok(store) => plugin.state.store = Some(Arc::new(store)),
        Err(err) => {
            return serde_json::json!({ "disable": format!("cannot open the payment history: {err:#}") })
        }
    }
    let ctx = match NetworkContext::new(network, data_dir, gossip_store.clone(), rgs_url.clone()) {
        Ok(ctx) => ctx,
        Err(err) => {
            return serde_json::json!({ "disable": format!("cannot initialize the network graph: {err}") })
        }
    };
    log::info!(
        "barq initialized on {network}: rapid gossip sync {}, gossip_store {}",
        rgs_url.as_deref().unwrap_or("disabled"),
        gossip_store
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "disabled".to_owned())
    );
    if let Some(path) = plugin.get_opt::<String>(OPT_DEV_SIMULATED_GRAPH) {
        let loaded = GraphSpec::from_file(&path).and_then(|spec| spec.apply(&ctx.graph));
        match loaded {
            Ok(count) => log::info!("loaded {count} simulated channels from {path}"),
            Err(err) => {
                return serde_json::json!({ "disable": format!("cannot load simulated graph {path}: {err:#}") })
            }
        }
    }
    if interval > 0 {
        ctx.start_background_sync(
            plugin.state.runtime.handle(),
            Duration::from_secs(interval as u64),
        );
    }
    plugin.state.ctx = Some(ctx);

    serde_json::json!({})
}

#[rpc_method(
    rpc_name = "barqpay",
    description = "Pay a BOLT11 or BOLT12 invoice routing with LDK"
)]
pub fn barq_pay(plugin: &mut Plugin<State>, request: Value) -> Result<Value, PluginError> {
    methods::pay::barq_pay(plugin, request)
}

#[rpc_method(
    rpc_name = "barqrouteinfo",
    description = "Get route information using Barq"
)]
fn barq_route_info(plugin: &mut Plugin<State>, request: Value) -> Result<Value, PluginError> {
    methods::route_info::barq_route_info(plugin, request)
}

#[rpc_method(
    rpc_name = "barqsyncgossip",
    description = "Refresh the LDK network graph from rapid gossip sync and the gossip_store"
)]
fn barq_sync_gossip(plugin: &mut Plugin<State>, request: Value) -> Result<Value, PluginError> {
    methods::sync::barq_sync_gossip(plugin, request)
}

/// lightningd asks subscribed plugins to exit on shutdown: save the graph
/// and the scorer, then exit without waiting for in-flight gossip syncs.
#[derive(Clone)]
struct OnShutdown;

impl RPCCommand<State> for OnShutdown {
    fn call_void(&self, plugin: &mut Plugin<State>, _request: &Value) {
        exit_plugin(&plugin.state, "lightningd is shutting down");
    }
}

pub(crate) fn exit_plugin(state: &State, reason: &str) -> ! {
    log::info!("barq exiting: {reason}");
    if let Some(ctx) = &state.ctx {
        if let Err(err) = ctx.persist() {
            log::warn!("failed to persist the network graph on exit: {err}");
        }
    }
    std::process::exit(0)
}

#[rpc_method(
    rpc_name = "barqinspect",
    description = "Inspect a past barq payment by id or payment_hash, or list recent ones"
)]
fn barq_inspect(plugin: &mut Plugin<State>, request: Value) -> Result<Value, PluginError> {
    methods::inspect::barq_inspect(plugin, request)
}
