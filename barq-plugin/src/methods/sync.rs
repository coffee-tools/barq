//! `barqsyncgossip`: refresh the LDK graph on demand and report its size.
use serde_json::{json, Value};

use clightningrpc_plugin::errors::PluginError;

use crate::plugin::State;

pub async fn barq_sync_gossip(state: State, _request: Value) -> Result<Value, PluginError> {
    let ctx = state.ctx()?;
    let report = ctx.sync_all().await;
    let (channels, nodes) = ctx.size();
    let rgs = match report.rapid_gossip_sync {
        None => json!({ "enabled": false }),
        Some(Ok(timestamp)) => json!({ "enabled": true, "last_sync_timestamp": timestamp }),
        Some(Err(err)) => json!({ "enabled": true, "error": err.to_string() }),
    };
    let gossip_store = match report.gossip_store {
        None => json!({ "enabled": false }),
        Some(Ok(stats)) => json!({
            "enabled": true,
            "channel_announcements": stats.channel_announcements,
            "channel_updates": stats.channel_updates,
            "node_announcements": stats.node_announcements,
            "rejected": stats.rejected,
            "offset": stats.offset,
        }),
        Some(Err(err)) => json!({ "enabled": true, "error": err.to_string() }),
    };
    Ok(json!({
        "channels": channels,
        "nodes": nodes,
        "rapid_gossip_sync": rgs,
        "gossip_store": gossip_store,
    }))
}
