//! Barq Plugin implementation

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use clightningrpc::LightningRPC;
use clightningrpc_plugin::commands::RPCCommand;
use clightningrpc_plugin::errors::PluginError;
use clightningrpc_plugin::plugin::Plugin;
use clightningrpc_plugin_macros::{plugin, rpc_method};

use crate::methods;

/// Barq Plugin State
///
/// This struct holds the router and CLN RPC path
/// to enable us to call CLN RPC methods
#[derive(Clone)]
pub(crate) struct State {
    /// CLN RPC path
    ///
    /// eg. /home/user/.lightning/lightning-rpc
    cln_rpc_path: Option<String>,
}

impl State {
    /// Create a new Barq Plugin State
    pub fn new() -> Self {
        State { cln_rpc_path: None }
    }

    /// Get CLN RPC path
    pub(crate) fn cln_rpc_path(&self) -> Option<String> {
        self.cln_rpc_path.clone()
    }

    /// A convenience method to call a CLN RPC method
    pub fn call<T: Serialize, U: DeserializeOwned + std::fmt::Debug>(
        &self,
        method: &str,
        payload: T,
    ) -> anyhow::Result<U> {
        let path = self
            .cln_rpc_path
            .as_ref()
            .ok_or(anyhow::anyhow!("cln socket path not found"))?;
        let rpc = LightningRPC::new(path);
        let response: U = rpc.call(method, payload)?;
        log::debug!("cln response: {:?}", response);
        Ok(response)
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
        ],
        hooks: [],
    };
    plugin.on_init(on_init);
    Ok(plugin)
}

/// This method is called when the plugin is initialized
fn on_init(plugin: &mut Plugin<State>) -> Value {
    let config = plugin.configuration.clone().unwrap();
    let rpc_file = format!("{}/{}", config.lightning_dir, config.rpc_file);

    plugin.state.cln_rpc_path = Some(rpc_file);

    serde_json::json!({})
}

#[rpc_method(rpc_name = "barqpay", description = "Execute a payment using Barq")]
pub fn barq_pay(plugin: &mut Plugin<State>, requet: Value) -> Result<Value, PluginError> {
    methods::pay::barq_pay(plugin, requet)
}

#[rpc_method(
    rpc_name = "barqrouteinfo",
    description = "Get route information using Barq"
)]
fn barq_route_info(plugin: &mut Plugin<State>, request: Value) -> Result<Value, PluginError> {
    methods::route_info::barq_route_info(plugin, request)
}
