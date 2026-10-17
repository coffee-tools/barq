//! `barqinspect`: look at past payments (issue #8).
use serde::Deserialize;
use serde_json::{json, Value};

use clightningrpc_plugin::error;
use clightningrpc_plugin::errors::PluginError;
use clightningrpc_plugin::plugin::Plugin;

use crate::plugin::State;

#[derive(Debug, Default, Deserialize)]
struct InspectRequest {
    /// The id `barqpay` returned.
    #[serde(default)]
    id: Option<u64>,
    /// Every payment to this hash.
    #[serde(default)]
    payment_hash: Option<String>,
    /// How many recent payments to list when neither is given.
    #[serde(default)]
    limit: Option<usize>,
}

impl InspectRequest {
    /// Accept `{"id": 3}` (`-k id=3`) as well as the positional `[3]`.
    fn parse(request: Value) -> Result<Self, PluginError> {
        match request {
            Value::Array(items) if items.is_empty() => Ok(Self::default()),
            Value::Array(items) => Ok(Self {
                id: Some(
                    items[0]
                        .as_u64()
                        .or_else(|| items[0].as_str().and_then(|s| s.parse().ok()))
                        .ok_or_else(|| error!("Expected a payment id, got {}", items[0]))?,
                ),
                ..Self::default()
            }),
            Value::Null => Ok(Self::default()),
            other => serde_json::from_value(other).map_err(|err| error!("{err}")),
        }
    }
}

pub fn barq_inspect(plugin: &mut Plugin<State>, request: Value) -> Result<Value, PluginError> {
    let request = InspectRequest::parse(request)?;
    let store = plugin.state.store()?;
    let failed = |err: anyhow::Error| error!("cannot read the payment history: {err:#}");

    if let Some(id) = request.id {
        let record = store
            .get(id)
            .map_err(failed)?
            .ok_or_else(|| error!("No payment with id {id}"))?;
        return Ok(serde_json::to_value(record)?);
    }
    if let Some(hash) = request.payment_hash {
        let records = store.by_payment_hash(&hash).map_err(failed)?;
        if records.is_empty() {
            return Err(error!("No payment with payment_hash {hash}"));
        }
        return Ok(json!({ "payments": records }));
    }
    let summaries = store.list(request.limit.unwrap_or(20)).map_err(failed)?;
    Ok(json!({ "payments": summaries }))
}
