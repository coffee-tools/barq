//! Setup shared by every command that routes a payment.
//!
//! `barqpay` and `barqrouteinfo` both need our node id, our usable channels
//! and a graph that includes the gossip Core Lightning received since the
//! last background sync. Loading it in one place removes the duplication
//! flagged in the review of PR #24 (issue #28).
use std::str::FromStr;

use clightningrpc_plugin::error;
use clightningrpc_plugin::errors::PluginError;

use barq_common::secp256k1::PublicKey;
use barq_common::strategy::{FirstHop, RouteInput, RouteOutput, StrategyKind};
use barq_common::Network;

use crate::methods::cln;
use crate::methods::pay::{build_strategy, strategy_name};
use crate::plugin::State;

/// Our node, as seen right before routing.
#[derive(Debug, Clone)]
pub struct NodeContext {
    pub our_id: PublicKey,
    pub network: Network,
    pub blockheight: u32,
    pub first_hops: Vec<FirstHop>,
}

/// Load our node info and channels and catch up with the gossip_store,
/// all concurrently.
pub async fn load(state: &State) -> Result<NodeContext, PluginError> {
    let ctx = state.ctx()?;
    let gossip_store = async {
        if ctx.gossip_store_path().is_some() {
            match ctx.sync_gossip_store_async().await {
                Ok(stats) => log::debug!("gossip_store sync before routing: {stats:?}"),
                Err(err) => log::warn!("gossip_store sync failed: {err}"),
            }
        }
        Ok::<(), PluginError>(())
    };
    let (info, first_hops, ()) =
        tokio::try_join!(cln::getinfo(state), cln::first_hops(state), gossip_store)?;
    Ok(NodeContext {
        our_id: PublicKey::from_str(&info.id).map_err(|_| error!("Invalid node id"))?,
        network: Network::from_str(&info.network).map_err(|e| error!("{e}"))?,
        blockheight: info.blockheight,
        first_hops,
    })
}

/// Check that strategy `kind` applies to `input` and compute its route.
pub fn route_with(
    state: &State,
    kind: StrategyKind,
    input: &RouteInput,
) -> Result<RouteOutput, PluginError> {
    let strategy = build_strategy(state, kind)?;
    if !strategy.can_apply(input).map_err(|e| error!("{e}"))? {
        return Err(error!(
            "Strategy `{}` cannot route this payment",
            strategy_name(kind)
        ));
    }
    let output = strategy.route(input).map_err(|err| error!("{err}"))?;
    if output.path.hops.is_empty() {
        return Err(error!("No route found to pay the invoice"));
    }
    Ok(output)
}
