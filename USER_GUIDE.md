# What is barq?

Barq is a Core Lightning plugin that pays BOLT11 and BOLT12 invoices using
[LDK](https://lightningdevkit.org) to find the path. Core Lightning still
owns the channels: barq asks the node for its usable channels, computes a
route with LDK's router and probabilistic scorer, builds the per-hop onion
payloads, and lets `lightningd` create and send the onion (`createonion` +
`sendonion`), then waits for the result with `waitsendpay`.

LDK needs a view of the network graph. Barq builds it from two sources and
keeps it (together with the scorer) in `<lightning-dir>/<network>/barq/`:

- **LDK rapid gossip sync (RGS)**: compact snapshots served by
  `rapidsync.lightningdevkit.org` (mainnet and testnet) or any self-hosted
  [`rapid-gossip-sync-server`](https://github.com/lightningdevkit/rapid-gossip-sync-server).
  Only the delta since the last sync is downloaded.
- **The Core Lightning `gossip_store`**: read incrementally, so the LDK
  graph also knows what `gossipd` learnt from peers. This is the only
  source on regtest/signet unless you point barq at your own RGS server.

## Rapid gossip sync caveats

A rapid gossip sync snapshot is a compressed, unsigned digest of the public
graph. It leaves out some information, which matters in these cases:

- **Freshness.** Snapshots are produced periodically by the server, and LDK
  backdates their updates by one week. New channels and recent fee or CLTV
  changes are missing until the next snapshot. Keep `barq-cln-gossip`
  enabled so the Core Lightning `gossip_store` fills the gap; its updates
  are newer and always win over the snapshot.
- **Private channels are not in snapshots.** BOLT11 payees reachable only
  through unannounced channels need route hints in the invoice. BOLT12 is
  unaffected because the blinded paths carry that information.
- **No node announcements.** Node features and addresses are missing. LDK
  only needs them to allow multi-part payments, which barq never does, so
  single-path routing is not affected.
- **No channel capacity.** The scorer falls back to the advertised
  `htlc_maximum_msat` as the channel size.
- **Staleness.** LDK refuses snapshots older than two weeks; the public
  testnet server currently serves such data. If the server is unreachable
  for about a week, the backdated channels are pruned from the graph.
- **Changing server.** The graph remembers the last snapshot timestamp of
  the previous server. After changing `barq-rgs-url`, delete
  `network_graph.<network>.bin` to force a full sync.
- **Networks.** testnet4 is not supported: barq disables itself there.

# How to build and install barq?

- clone the repository
  `git clone https://github.com/coffee-tools/barq.git`
- build project
  `make`
- run lightningd with the plugin, or install it with
  [coffee](https://github.com/coffee-tools/coffee):
  `lightningd ... --plugin=<path-barq-repo>/target/debug/barq-plugin`

# Options

| Option                  | Default | Description                                                                                                                  |
| ----------------------- | ------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `barq-rgs-url`          | `auto`  | RGS snapshot endpoint without the trailing timestamp. `auto` uses the public LDK server on mainnet/testnet, `off` disables RGS. |
| `barq-sync-interval`    | `3600`  | Seconds between background refreshes of the graph (RGS + gossip_store). `0` disables the background sync.                     |
| `barq-cln-gossip`       | `true`  | Feed the LDK graph with the Core Lightning `gossip_store`.                                                                    |

Example for a regtest node with a self-hosted RGS server:

```
lightningd --network=regtest --plugin=.../barq-plugin \
  --barq-rgs-url=http://127.0.0.1:8080/snapshot --barq-sync-interval=600
```

## Testing strategies on simulated networks

`barq-common` can build LDK graphs of simulated nodes and channels
(`barq_common::simulation`): lines, grids and seeded random graphs, with a
fee policy per channel direction. Use it to unit-test a strategy, or to
benchmark one:

```
cargo run --release -p barq-common --example routing_bench -- 2000 6000 100
```

The developer option `barq-dev-simulated-graph=<file.json>` loads a mocked
graph into the plugin at startup, next to the real one, so integration
tests can route over topologies much larger than a regtest network. The
file holds `{"channels": [...]}` where each channel has `node1`, `node2`
(hex node ids), `capacity_sat`, and optionally `short_channel_id`,
`policy_1to2` and `policy_2to1` (`fee_base_msat`,
`fee_proportional_millionths`, `cltv_expiry_delta`, `htlc_minimum_msat`,
`htlc_maximum_msat`, `disabled`). Payments cannot settle over mocked nodes.
See `tests/test_simulation.py`.

# Barq commands

`lightning-cli --network=<network-name> -k <barq-command> <param>=<value> ...`

## `barqpay`

Pay a BOLT11 or BOLT12 invoice.

| Parameter               | Required | Description                                                                                 |
| ----------------------- | -------- | ------------------------------------------------------------------------------------------- |
| `invoice`               | yes      | The BOLT11 or BOLT12 invoice (`bolt11_invoice` is still accepted).                          |
| `amount_msat`           | no       | Amount to pay when the invoice has none.                                                    |
| `strategy`              | no       | `probabilistic` (default, LDK path finding) or `direct` (pay a peer over a shared channel). |
| `use_rapid_gossip_sync` | no       | Refresh the graph from the RGS server right before routing.                                 |
| `label`                 | no       | Label stored with the payment.                                                              |

The response reports the status, the preimage, the route (`id`, `channel`,
`fee_msat`, `cltv_expiry_delta` per hop), the number of blinded hops for
BOLT12, the fee paid and the raw `waitsendpay` result.

```
lightning-cli -k barqpay invoice=lnbc...
lightning-cli -k barqpay invoice=lni1... 
lightning-cli -k barqpay invoice=lnbc... strategy=direct
lightning-cli -k barqpay invoice=lnbc... use_rapid_gossip_sync=true
```

Barq always pays with a single HTLC (part id 0, `total_msat` equal to the
amount). It never splits a payment into multiple parts, even when the invoice
advertises `basic_mpp`. If no single path can carry the amount, `barqpay`
fails with `Failed to find route` before anything is sent.

On failure `barqpay` returns the `waitsendpay` error. Because the onion is
built by barq, Core Lightning cannot know the route, so barq fills in:

- `erring_node` and `erring_channel`, mapped from `erring_index` onto the
  route LDK chose;
- `erring_in_blinded_path`, true when the failure came from the BOLT12
  blinded path (where errors are masked by design);
- code `203` instead of `204` when the destination itself rejected a
  BOLT11 payment, so callers know a different route will not help.

The failing channel is penalised in the LDK scorer, which is persisted.

A BOLT12 invoice is obtained as usual with `fetchinvoice`; barq routes to
the introduction node of one of its blinded paths and builds the blinded
payloads itself.

## `barqrouteinfo`

Compute a route without paying: `dest_pubkey`, `amount_msat`, optional
`cltv` (final CLTV delta, default 18) and `strategy`.

## `barqsyncgossip`

Refresh the LDK graph from every configured source right now and report
how many channels and nodes it knows, the RGS timestamp and the
gossip_store counters. Useful to check the RGS configuration.
