//! Serve Core Lightning's own `pay` command with barq (issue #36).
//!
//! barq registers an `rpc_command` hook filtered to `pay`. Without the
//! filter the hook would see every command on the node, including the ones
//! barq sends while paying, and barq (which serves one request at a time)
//! would wait for itself. Core Lightning supports `rpc_command` filters
//! since v25.12 and refuses filters on older versions, so the hook is only
//! registered there.
//!
//! With `barq-override-pay=true`, a `pay` call is paid by barq and answered
//! in `pay`'s format, so existing callers and test suites work unchanged.
//! Otherwise the hook lets Core Lightning's `pay` run.
//!
//! Since v25.12 `cln-xpay` takes over `pay` the same way when
//! `xpay-handle-pay` is set (the default). Two plugins answering the same
//! `rpc_command` crash Core Lightning (a double free in the chained hook,
//! seen on v26.06.9), so barq only takes over when xpay does not.
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use clightningrpc_plugin::commands::RPCCommand;
use clightningrpc_plugin::errors::PluginError;
use clightningrpc_plugin::plugin::Plugin;

use crate::methods::cln;
use crate::methods::pay::{self, BarqPayRequest};
use crate::plugin::State;

/// First Core Lightning version with `rpc_command` filters.
pub const FILTERS_SINCE: (u32, u32) = (25, 12);

/// `pay` error codes (see `lightning-pay(7)`).
const PAY_DESTINATION_PERM_FAIL: i64 = 203;
const PAY_TRY_OTHER_ROUTE: i64 = 204;
const PAY_ROUTE_NOT_FOUND: i64 = 205;
const PAY_STOPPED_RETRYING: i64 = 210;
const JSONRPC2_INVALID_PARAMS: i64 = -32602;

/// `(major, minor)` of a Core Lightning version string such as `v26.06.9`,
/// `v25.12rc1` or `v24.02.2-123-gabcdef`.
pub fn parse_version(version: &str) -> Option<(u32, u32)> {
    let version = version.trim().trim_start_matches('v');
    let mut parts = version.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Whether this lightningd accepts an `rpc_command` hook filtered to `pay`.
pub fn supports_filters(version: Option<&str>) -> bool {
    version
        .and_then(parse_version)
        .is_some_and(|v| v >= FILTERS_SINCE)
}

/// Parse a Core Lightning amount: a number of msat, or a string ending in
/// `msat`, `sat` or `btc`.
pub fn parse_amount_msat(value: &Value) -> Result<u64, String> {
    if let Some(msat) = value.as_u64() {
        return Ok(msat);
    }
    let Some(text) = value.as_str() else {
        return Err(format!("invalid amount {value}"));
    };
    let invalid = || format!("invalid amount `{text}`");
    if let Some(msat) = text.strip_suffix("msat") {
        return msat.parse().map_err(|_| invalid());
    }
    if let Some(sat) = text.strip_suffix("sat") {
        return sat
            .parse::<u64>()
            .ok()
            .and_then(|s| s.checked_mul(1000))
            .ok_or_else(invalid);
    }
    if let Some(btc) = text.strip_suffix("btc") {
        // 1 btc = 10^11 msat; at most 11 decimals.
        let (whole, frac) = btc.split_once('.').unwrap_or((btc, ""));
        if frac.len() > 11 || (whole.is_empty() && frac.is_empty()) {
            return Err(invalid());
        }
        let whole: u64 = if whole.is_empty() {
            0
        } else {
            whole.parse().map_err(|_| invalid())?
        };
        let frac: u64 = if frac.is_empty() {
            0
        } else {
            format!("{frac:0<11}").parse().map_err(|_| invalid())?
        };
        return whole
            .checked_mul(100_000_000_000)
            .and_then(|w| w.checked_add(frac))
            .ok_or_else(invalid);
    }
    text.parse().map_err(|_| invalid())
}

/// Translate `pay`'s parameters (by name or by position) into a `barqpay`
/// request. Parameters barq has no equivalent for are ignored.
pub fn barqpay_request(params: &Value) -> Result<BarqPayRequest, String> {
    let (bolt11, amount, label) = match params {
        Value::Object(map) => (
            map.get("bolt11").cloned(),
            map.get("amount_msat").cloned(),
            map.get("label").cloned(),
        ),
        // pay's positional order: bolt11, amount_msat, label, ...
        Value::Array(items) => (
            items.first().cloned(),
            items.get(1).cloned(),
            items.get(2).cloned(),
        ),
        _ => (None, None, None),
    };
    let invoice = bolt11
        .as_ref()
        .and_then(Value::as_str)
        .ok_or("missing `bolt11`")?
        .to_owned();
    let amount_msat = match amount {
        None | Some(Value::Null) => None,
        Some(value) => Some(parse_amount_msat(&value)?),
    };
    let label = match label {
        None | Some(Value::Null) => None,
        Some(Value::String(label)) => Some(label),
        Some(other) => Some(other.to_string()),
    };
    Ok(BarqPayRequest {
        invoice,
        amount_msat,
        strategy: None,
        use_rapid_gossip_sync: false,
        label,
        max_attempts: None,
    })
}

/// A `barqpay` result in `pay`'s format.
pub fn pay_result(barq: &Value, created_at: f64) -> Value {
    let mut result = json!({
        "payment_hash": barq["payment_hash"],
        "created_at": created_at,
        "parts": 1,
        "amount_msat": barq["amount_msat"],
        "amount_sent_msat": barq["amount_sent_msat"],
        "payment_preimage": barq["payment_preimage"],
        "status": "complete",
    });
    if let Some(destination) = barq.get("destination") {
        result["destination"] = destination.clone();
    }
    result
}

/// A `barqpay` error with `pay`'s error codes. The data keeps barq's
/// details (`id`, `attempts`, ...).
pub fn pay_error(barq: &Value) -> Value {
    let message = barq["message"].as_str().unwrap_or("barq payment failed");
    let code = match barq["code"].as_i64().unwrap_or(-1) {
        PAY_DESTINATION_PERM_FAIL => PAY_DESTINATION_PERM_FAIL,
        PAY_TRY_OTHER_ROUTE => PAY_STOPPED_RETRYING,
        _ if message.starts_with("No route found") || message.starts_with("No usable channel") => {
            PAY_ROUTE_NOT_FOUND
        }
        code => code,
    };
    let mut error = json!({ "code": code, "message": message });
    if !barq["data"].is_null() {
        error["data"] = barq["data"].clone();
    }
    error
}

/// The `rpc_command` hook handler.
#[derive(Clone)]
pub struct PayHook;

impl RPCCommand<State> for PayHook {
    fn call<'c>(&self, plugin: &mut Plugin<State>, request: Value) -> Result<Value, PluginError> {
        let command = &request["rpc_command"];
        if command["method"] != "pay" || !plugin.state.override_pay {
            return Ok(json!({ "result": "continue" }));
        }
        // If xpay also takes over `pay`, two plugins modify the same command
        // and Core Lightning (at least up to v26.06.9) frees the request
        // twice and aborts. Never risk the node: leave `pay` to xpay.
        let state = plugin.state.clone();
        if plugin.state.block_on(cln::xpay_handles_pay(&state)) {
            log::warn!(
                "barq-override-pay is set but xpay also handles `pay` \
                 (xpay-handle-pay=true); letting xpay pay. Set xpay-handle-pay=false \
                 for barq to serve `pay`."
            );
            return Ok(json!({ "result": "continue" }));
        }
        log::info!("serving `pay` with barq: {}", command["params"]);
        Ok(match serve_pay(plugin, &command["params"]) {
            Ok(result) => json!({ "return": { "result": result } }),
            Err(error) => json!({ "return": { "error": error } }),
        })
    }
}

/// Pay with barq through `pay`'s interface: `pay`'s parameters (by name or
/// position), `pay`'s result format and `pay`'s error codes. Used by the
/// `pay` hook and by the `rpay` command.
pub fn serve_pay(plugin: &mut Plugin<State>, params: &Value) -> Result<Value, Value> {
    let pay_request = barqpay_request(params)
        .map_err(|message| json!({ "code": JSONRPC2_INVALID_PARAMS, "message": message }))?;
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let state = plugin.state.clone();
    match plugin.state.block_on(pay::pay(state, pay_request)) {
        Ok(result) => Ok(pay_result(&result, created_at)),
        Err(err) => Err(pay_error(&serde_json::to_value(&err).unwrap_or_default())),
    }
}

/// The `rpay` command: `pay`'s interface, always served by barq.
pub fn rpay(plugin: &mut Plugin<State>, params: Value) -> Result<Value, PluginError> {
    log::info!("rpay called with request: {params}");
    serve_pay(plugin, &params).map_err(|error| {
        PluginError::new(
            error["code"].as_i64().unwrap_or(-1) as i32,
            error["message"].as_str().unwrap_or("barq payment failed"),
            error.get("data").cloned(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v26.06.9"), Some((26, 6)));
        assert_eq!(parse_version("v25.12rc1"), Some((25, 12)));
        assert_eq!(parse_version("v24.02.2-123-gabcdef"), Some((24, 2)));
        assert_eq!(parse_version("25.09"), Some((25, 9)));
        assert_eq!(parse_version("garbage"), None);
        assert!(supports_filters(Some("v26.06.9")));
        assert!(supports_filters(Some("v25.12")));
        assert!(!supports_filters(Some("v25.09")));
        assert!(!supports_filters(Some("v24.11.1")));
        assert!(!supports_filters(None));
    }

    #[test]
    fn amounts() {
        assert_eq!(parse_amount_msat(&json!(1234)), Ok(1234));
        assert_eq!(parse_amount_msat(&json!("1234msat")), Ok(1234));
        assert_eq!(parse_amount_msat(&json!("12sat")), Ok(12_000));
        assert_eq!(parse_amount_msat(&json!("0.001btc")), Ok(100_000_000));
        assert_eq!(parse_amount_msat(&json!("1btc")), Ok(100_000_000_000));
        assert_eq!(parse_amount_msat(&json!("0.00000000001btc")), Ok(1));
        assert_eq!(parse_amount_msat(&json!("5000")), Ok(5000));
        assert!(parse_amount_msat(&json!("12xyz")).is_err());
        assert!(parse_amount_msat(&json!("0.000000000001btc")).is_err());
        assert!(parse_amount_msat(&json!(true)).is_err());
    }

    #[test]
    fn requests_by_name_and_by_position() {
        let by_name = barqpay_request(&json!({
            "bolt11": "lnbc1",
            "amount_msat": "10sat",
            "label": "coffee",
            "maxfeepercent": 0.5,
        }))
        .unwrap();
        assert_eq!(by_name.invoice, "lnbc1");
        assert_eq!(by_name.amount_msat, Some(10_000));
        assert_eq!(by_name.label.as_deref(), Some("coffee"));

        let by_position = barqpay_request(&json!(["lnbc2", 5000])).unwrap();
        assert_eq!(by_position.invoice, "lnbc2");
        assert_eq!(by_position.amount_msat, Some(5000));
        assert_eq!(by_position.label, None);

        assert!(barqpay_request(&json!({})).is_err());
        assert!(barqpay_request(&json!({"bolt11": "x", "amount_msat": "bad"})).is_err());
    }

    #[test]
    fn results_and_errors_use_pay_format() {
        let barq = json!({
            "id": 7,
            "payment_hash": "aa",
            "payment_preimage": "bb",
            "destination": "02cc",
            "amount_msat": 1000,
            "amount_sent_msat": 1001,
            "strategy": "probabilistic",
        });
        let result = pay_result(&barq, 1.5);
        assert_eq!(result["status"], "complete");
        assert_eq!(result["parts"], 1);
        assert_eq!(result["amount_sent_msat"], 1001);
        assert_eq!(result["destination"], "02cc");
        assert!(result.get("strategy").is_none());

        let code = |code: i64, message: &str| {
            pay_error(&json!({"code": code, "message": message}))["code"].clone()
        };
        assert_eq!(code(203, "failed: WIRE_INCORRECT"), json!(203));
        assert_eq!(code(204, "failed: WIRE_TEMPORARY"), json!(210));
        assert_eq!(
            code(-1, "No route found to pay the invoice: ..."),
            json!(205)
        );
        assert_eq!(code(-1, "No usable channel to pay from"), json!(205));
        assert_eq!(code(-1, "something else"), json!(-1));
        let with_data = pay_error(&json!({"code": 203, "message": "m", "data": {"id": 3}}));
        assert_eq!(with_data["data"]["id"], 3);
    }
}
