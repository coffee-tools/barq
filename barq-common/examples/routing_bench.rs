//! Benchmark the LDK strategy on a simulated random network.
//!
//! ```shell
//! cargo run --release -p barq-common --example routing_bench -- 2000 6000 100
//! ```
//!
//! Arguments: number of nodes, extra random channels, number of payments.
use std::time::Instant;

use barq_common::algorithms::probabilistic::LDKRoutingStrategy;
use barq_common::simulation::SimulatedNetwork;
use barq_common::strategy::Strategy;

fn arg(index: usize, default: usize) -> usize {
    std::env::args()
        .nth(index)
        .and_then(|a| a.parse().ok())
        .unwrap_or(default)
}

fn main() -> anyhow::Result<()> {
    let (nodes, extra, payments) = (arg(1, 1000), arg(2, 3000), arg(3, 100));

    let start = Instant::now();
    let net = SimulatedNetwork::random(nodes, extra, 42)?;
    let (channels, known_nodes) = net.ctx.size();
    println!(
        "graph: {known_nodes} nodes, {channels} channels, built in {:?}",
        start.elapsed()
    );

    let strategy = LDKRoutingStrategy::new(net.ctx.clone());
    let (mut ok, mut hops, mut fees) = (0usize, 0usize, 0u64);
    let start = Instant::now();
    for i in 0..payments {
        let to = 1 + (i * 7919) % (nodes - 1);
        if let Ok(route) = strategy.route(&net.route_input(0, to, 50_000)) {
            ok += 1;
            hops += route.path.hops.len();
            fees += route.fee_msat();
        }
    }
    let elapsed = start.elapsed();
    println!(
        "{payments} payments: {ok} routed, {:?} per route, avg {:.1} hops, avg fee {} msat",
        elapsed / payments.max(1) as u32,
        hops as f64 / ok.max(1) as f64,
        fees / ok.max(1) as u64
    );
    Ok(())
}
