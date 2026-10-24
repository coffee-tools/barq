//! Strategies written in any language (issue #12).
//!
//! An external strategy is a program. For every route barq needs, it runs
//! the program, writes one JSON request on its stdin and reads one JSON
//! response from its stdout. The program runs in the node's lightning-dir,
//! so it can query Core Lightning through `rpc_file` for whatever data its
//! algorithm needs. barq validates the route before using it.
//!
//! Request:
//!
//! ```json
//! {"version": 1, "source": "02..", "destination": "03..",
//!  "amount_msat": 1000, "final_cltv_delta": 18,
//!  "first_hops": [{"peer_id": "02..", "short_channel_id": "103x1x0",
//!                  "spendable_msat": 1, "htlc_minimum_msat": 1,
//!                  "htlc_maximum_msat": 1}],
//!  "excluded_channels": ["104x1x0"], "rpc_file": "lightning-rpc"}
//! ```
//!
//! Response: `{"route": [{"id", "channel", "fee_msat", "cltv_expiry_delta"}]}`
//! with the same meaning as an LDK route hop, or `{"error": "..."}`.
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::ldk::ln::features::{ChannelFeatures, NodeFeatures};
use crate::ldk::routing::router::{Path, RouteHop};
use crate::scid;
use crate::secp256k1::PublicKey;
use crate::strategy::{Payee, RouteInput, RouteOutput, Strategy};

pub const PROTOCOL_VERSION: u64 = 1;

/// A strategy implemented by an external program.
#[derive(Debug, Clone)]
pub struct ExternalStrategy {
    pub program: PathBuf,
    /// Working directory of the program: the lightning-dir.
    pub workdir: Option<PathBuf>,
    /// Core Lightning's RPC socket, relative to `workdir`.
    pub rpc_file: String,
    pub timeout: Duration,
}

impl ExternalStrategy {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            workdir: None,
            rpc_file: "lightning-rpc".to_owned(),
            timeout: Duration::from_secs(30),
        }
    }

    /// The JSON request sent to the program.
    pub fn request(&self, input: &RouteInput) -> Result<Value> {
        let Payee::Clear { node_id, .. } = &input.payee else {
            anyhow::bail!("external strategies cannot pay blinded paths");
        };
        Ok(json!({
            "version": PROTOCOL_VERSION,
            "source": input.src_pubkey.to_string(),
            "destination": node_id.to_string(),
            "amount_msat": input.amount_msat,
            "final_cltv_delta": input.final_cltv_delta,
            "first_hops": input.first_hops.iter().map(|hop| json!({
                "peer_id": hop.peer_id.to_string(),
                "short_channel_id": scid::to_cln(hop.scid),
                "spendable_msat": hop.spendable_msat,
                "htlc_minimum_msat": hop.htlc_minimum_msat,
                "htlc_maximum_msat": hop.htlc_maximum_msat,
            })).collect::<Vec<_>>(),
            "excluded_channels": input
                .excluded_channels
                .iter()
                .map(|s| scid::to_cln(*s))
                .collect::<Vec<_>>(),
            "rpc_file": self.rpc_file,
        }))
    }

    /// Run the program on `request` and return its JSON response.
    fn run(&self, request: &Value) -> Result<Value> {
        let mut command = Command::new(&self.program);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = &self.workdir {
            command.current_dir(dir);
        }
        let mut child = spawn_retrying_busy(&mut command)
            .with_context(|| format!("cannot run {}", self.program.display()))?;
        {
            let mut stdin = child.stdin.take().expect("stdin is piped");
            // A program that does not read its input closes the pipe early;
            // that is its business, its answer is what matters.
            let _ = stdin.write_all(request.to_string().as_bytes());
        }
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!(
                    "{} timed out after {:?}",
                    self.program.display(),
                    self.timeout
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut stdout = String::new();
        let mut stderr = String::new();
        if let Some(mut out) = child.stdout.take() {
            out.read_to_string(&mut stdout)?;
        }
        if let Some(mut err) = child.stderr.take() {
            err.read_to_string(&mut stderr)?;
        }
        if !status.success() {
            anyhow::bail!(
                "{} failed ({status}): {}",
                self.program.display(),
                stderr.trim()
            );
        }
        serde_json::from_str(stdout.trim()).with_context(|| {
            format!(
                "{} did not print a JSON response: {:?}",
                self.program.display(),
                stdout.trim()
            )
        })
    }
}

/// Check the program's response and turn it into an LDK path.
pub fn route_from_response(input: &RouteInput, response: &Value) -> Result<Path> {
    if let Some(error) = response.get("error") {
        anyhow::bail!("{}", error.as_str().unwrap_or(&error.to_string()));
    }
    let Some(route) = response.get("route").and_then(Value::as_array) else {
        anyhow::bail!("the response has neither `route` nor `error`");
    };
    if route.is_empty() {
        anyhow::bail!("the route is empty");
    }
    let mut hops = Vec::with_capacity(route.len());
    for (index, hop) in route.iter().enumerate() {
        let field = |name: &str| {
            hop.get(name)
                .with_context(|| format!("hop {index} has no `{name}`"))
        };
        let id = field("id")?
            .as_str()
            .with_context(|| format!("hop {index}: `id` is not a string"))?;
        let channel = field("channel")?
            .as_str()
            .with_context(|| format!("hop {index}: `channel` is not a string"))?;
        let short_channel_id =
            scid::from_cln(channel).with_context(|| format!("hop {index}: bad channel"))?;
        if input.excluded_channels.contains(&short_channel_id) {
            anyhow::bail!("hop {index} uses excluded channel {channel}");
        }
        hops.push(RouteHop {
            pubkey: PublicKey::from_str(id)
                .map_err(|_| anyhow::anyhow!("hop {index}: bad node id `{id}`"))?,
            node_features: NodeFeatures::empty(),
            short_channel_id,
            channel_features: ChannelFeatures::empty(),
            fee_msat: field("fee_msat")?
                .as_u64()
                .with_context(|| format!("hop {index}: bad `fee_msat`"))?,
            cltv_expiry_delta: field("cltv_expiry_delta")?
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .with_context(|| format!("hop {index}: bad `cltv_expiry_delta`"))?,
            maybe_announced_channel: true,
        });
    }

    let first = &hops[0];
    let Some(first_hop) = input
        .first_hops
        .iter()
        .find(|h| h.scid == first.short_channel_id && h.peer_id == first.pubkey)
    else {
        anyhow::bail!(
            "the first hop {} is not one of our channels",
            scid::to_cln(first.short_channel_id)
        );
    };
    hops[0].maybe_announced_channel = first_hop.is_public;
    let last = hops.last().expect("not empty");
    if Some(last.pubkey) != input.payee.node_id() {
        anyhow::bail!("the route does not end at the destination");
    }
    if last.fee_msat != input.amount_msat {
        anyhow::bail!(
            "the last hop delivers {} msat instead of {}",
            last.fee_msat,
            input.amount_msat
        );
    }
    Ok(Path {
        hops,
        blinded_tail: None,
    })
}

impl Strategy for ExternalStrategy {
    fn can_apply(&self, input: &RouteInput) -> Result<bool> {
        Ok(matches!(input.payee, Payee::Clear { .. }) && !input.first_hops.is_empty())
    }

    fn route(&self, input: &RouteInput) -> Result<RouteOutput> {
        let request = self.request(input)?;
        let response = self.run(&request)?;
        Ok(RouteOutput {
            path: route_from_response(input, &response)?,
        })
    }
}

/// `ETXTBSY`: the program file is still open for writing somewhere.
const ETXTBSY: i32 = 26;

/// Spawn `command`, retrying for a short while when the kernel refuses to
/// execute the program because its file is still open for writing
/// ("Text file busy"). That happens when the program was just written and
/// another thread of this process forked in between: the child holds the
/// writing descriptor until it execs. barq's own tests write scripts while
/// others run, and an external strategy can be replaced while barq runs.
fn spawn_retrying_busy(command: &mut Command) -> std::io::Result<std::process::Child> {
    let mut tries = 0;
    loop {
        match command.spawn() {
            Err(err) if err.raw_os_error() == Some(ETXTBSY) && tries < 50 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::simulation::{Policy, SimulatedNetwork};

    fn script(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("barq-external-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn line() -> SimulatedNetwork {
        SimulatedNetwork::line(3, 1_000_000, Policy::default()).unwrap()
    }

    fn hop(id: PublicKey, channel: u64, fee: u64, cltv: u32) -> Value {
        json!({"id": id.to_string(), "channel": scid::to_cln(channel), "fee_msat": fee, "cltv_expiry_delta": cltv})
    }

    #[test]
    fn request_describes_the_payment() {
        let net = line();
        let mut input = net.route_input(0, 2, 5_000);
        input.excluded_channels = vec![net.channels()[1].scid];
        let request = ExternalStrategy::new("x").request(&input).unwrap();
        assert_eq!(request["version"], 1);
        assert_eq!(request["destination"], net.node_id(2).to_string());
        assert_eq!(request["amount_msat"], 5_000);
        assert_eq!(
            request["first_hops"][0]["peer_id"],
            net.node_id(1).to_string()
        );
        assert_eq!(
            request["excluded_channels"][0],
            scid::to_cln(net.channels()[1].scid)
        );
    }

    #[test]
    fn valid_two_hop_route() {
        let net = line();
        let input = net.route_input(0, 2, 5_000);
        let (a, b) = (net.channels()[0].scid, net.channels()[1].scid);
        let response =
            json!({"route": [hop(net.node_id(1), a, 1000, 6), hop(net.node_id(2), b, 5000, 18)]});
        let path = route_from_response(&input, &response).unwrap();
        assert_eq!(path.hops.len(), 2);
        assert_eq!(path.fee_msat(), 1000);
    }

    #[test]
    fn invalid_routes_are_rejected() {
        let net = line();
        let input = net.route_input(0, 2, 5_000);
        let (a, b) = (net.channels()[0].scid, net.channels()[1].scid);
        let check = |response: Value, expected: &str| {
            let err = route_from_response(&input, &response)
                .unwrap_err()
                .to_string();
            assert!(err.contains(expected), "{err} does not contain {expected}");
        };
        check(json!({"error": "nope"}), "nope");
        check(json!({}), "neither");
        check(json!({"route": []}), "empty");
        // First hop is not one of our channels.
        check(
            json!({"route": [hop(net.node_id(2), b, 5000, 18)]}),
            "not one of our channels",
        );
        // Does not reach the destination.
        check(
            json!({"route": [hop(net.node_id(1), a, 5000, 18)]}),
            "does not end",
        );
        // Wrong amount delivered.
        check(
            json!({"route": [hop(net.node_id(1), a, 1000, 6), hop(net.node_id(2), b, 4000, 18)]}),
            "delivers 4000",
        );
        check(
            json!({"route": [{"id": "x", "channel": "1x1x1", "fee_msat": 1, "cltv_expiry_delta": 1}]}),
            "bad node id",
        );
        check(
            json!({"route": [{"id": net.node_id(1).to_string(), "channel": "bad"}]}),
            "bad channel",
        );

        let mut excluded = input.clone();
        excluded.excluded_channels = vec![b];
        let err = route_from_response(
            &excluded,
            &json!({"route": [hop(net.node_id(1), a, 1000, 6), hop(net.node_id(2), b, 5000, 18)]}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("excluded"));
    }

    #[test]
    fn runs_a_program() {
        let net = line();
        let input = net.route_input(0, 1, 7_000);
        let response = json!({"route": [hop(net.node_id(1), net.channels()[0].scid, 7000, 18)]});
        let program = script("ok.sh", &format!("cat > /dev/null\necho '{response}'"));
        let route = ExternalStrategy::new(program).route(&input).unwrap();
        assert_eq!(route.path.hops[0].pubkey, net.node_id(1));
    }

    #[test]
    fn program_receives_the_request() {
        let net = line();
        let input = net.route_input(0, 1, 7_000);
        // Echo the destination back as an error to prove the input arrived.
        let program = script(
            "echo.sh",
            r#"read line; dest=$(echo "$line" | sed 's/.*"destination":"\([0-9a-f]*\)".*/\1/'); echo "{\"error\": \"dest $dest\"}""#,
        );
        let err = ExternalStrategy::new(program).route(&input).unwrap_err();
        assert_eq!(err.to_string(), format!("dest {}", net.node_id(1)));
    }

    #[test]
    fn program_failures_are_reported() {
        let net = line();
        let input = net.route_input(0, 1, 7_000);

        let crash = script("crash.sh", "echo boom >&2; exit 3");
        let err = ExternalStrategy::new(crash)
            .route(&input)
            .unwrap_err()
            .to_string();
        assert!(err.contains("boom"), "{err}");

        let garbage = script("garbage.sh", "echo not json");
        let err = ExternalStrategy::new(garbage)
            .route(&input)
            .unwrap_err()
            .to_string();
        assert!(err.contains("did not print a JSON response"), "{err}");

        let slow = script("slow.sh", "sleep 5");
        let mut strategy = ExternalStrategy::new(slow);
        strategy.timeout = Duration::from_millis(200);
        let err = strategy.route(&input).unwrap_err().to_string();
        assert!(err.contains("timed out"), "{err}");

        let missing = ExternalStrategy::new("/nonexistent/strategy");
        assert!(missing
            .route(&input)
            .unwrap_err()
            .to_string()
            .contains("cannot run"));
    }

    #[test]
    fn blinded_payees_are_not_supported() {
        let net = line();
        let mut input = net.route_input(0, 1, 7_000);
        input.payee = Payee::Blinded {
            paths: vec![],
            features: None,
        };
        let strategy = ExternalStrategy::new("x");
        assert!(!strategy.can_apply(&input).unwrap());
        assert!(strategy.request(&input).is_err());
    }
}
