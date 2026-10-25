# barq on Mutinynet, end to end

Three Core Lightning nodes on [Mutinynet](https://mutinynet.com), the signet
with 30 second blocks: **alice** runs barq, **carol** routes, **bob** gets
paid. `signet-sim.sh` opens alice → carol → bob and checks, on the real
network:

- BOLT11 payments over two hops, with `barqpay` and with `rpay`
- BOLT12 payments of a fixed-amount and an any-amount offer (invoices
  fetched over onion messages, paid through bob's blinded path)
- failures: an unknown invoice and a used single-use offer (203), an
  unroutable amount (205), the fee limit (206)
- a self-payment, an invoice paid twice, `pay`-style attempt groups
- the payment history (`barqinspect`)
- `pay` itself served by barq (`barq-override-pay`, xpay's takeover off)

## Setup

Everything lives in one directory (`ROOT`, default `~/barq-signet`):

| Path | What |
| --- | --- |
| `cln/usr` | Core Lightning (v25.12 or later for the `pay` hook) |
| `barq` | this repository, built with `cargo build --release` |
| `nodes.sh`, `signet-sim.sh` | copied from here |

`nodes.sh` talks to a Mutinynet bitcoind on `127.0.0.1:38332` (edit
`write_config` for yours) and gives alice barq with the Mutinynet rapid
gossip sync server.

```bash
./nodes.sh start
./nodes.sh cli alice newaddr
./nodes.sh cli carol newaddr
```

Fund both addresses from the faucet, for example with
[mutinynet-cli](https://github.com/benthecarman/mutinynet-cli)
(`mutinynet-cli login` uses GitHub's device flow):

```bash
mutinynet-cli onchain <alice address> 300000
mutinynet-cli onchain <carol address> 200000
```

Then:

```bash
./signet-sim.sh          # wait for funds, open the channels, run every check
./signet-sim.sh checks   # the checks only, once the channels exist
```

Each check prints PASS or FAIL in `signet-sim.log`; the exit status is
non-zero when one fails.

## Rapid gossip sync

alice normally reads both Core Lightning's gossip_store and the Mutinynet
rapid gossip sync server (`https://rgs.mutinynet.com/snapshot`). The `rgs`
checks restart alice with `barq-cln-gossip=false`, so every channel barq
routes over, beyond alice's own, comes from the snapshots:

- a full sync from an empty graph holding every channel of the server's
  full snapshot (Mutinynet's server knows fewer channels than Core
  Lightning's gossip)
- the next sync continuing from the last applied timestamp, and a restart
  continuing from the persisted one
- routes to a sample of public nodes compared with Core Lightning's
  `getroute` (`RGS_SAMPLE`, default 40)
- BOLT11 and BOLT12 payments, a 205 failure, concurrent payments and the
  faucet's node, all routed over RGS data
- an unreachable server (the gossip_store takes over), another chain's
  server and a server answering something else

```bash
./signet-sim.sh rgs           # once the channels of `all` and `public` exist
./signet-sim.sh rgs-soak 12   # 12 hours of syncs, routes and payments
```

Both put alice back on its usual configuration when they end.

## Paying an LDK node

The `lampo` checks pay a [lampo](https://github.com/vincenzopalazzo/lampo.rs)
node, an LDK implementation, that alice has no channel with: carol opens a
channel to it, and so does the faucet's node (`mutinynet-cli channel
<lampo id> <sats> --push-amount 0`, once lampo is connected to it). Run
lampo with `network=signet` against the same bitcoind, and point
`LAMPO_CLI` at a script that runs `lampo-cli --url <its API>`. The checks
pay BOLT11 invoices (with an amount, without one, with `rpay`), BOLT12
offers, an expired invoice (207), two invoices at once, and one through the
faucet with carol excluded. lampo sends no node announcement without a public
address, so the payer fetches BOLT12 invoices over a plain peer connection
to lampo (`LAMPO_ADDR`, no channel). Every payment must be routed by LDK's router (barq's
`probabilistic` strategy). With `PAYER=bob` (bob started with
`BARQ_NODES="alice bob"`) the payer reads RGS and Core Lightning's gossip;
with `RGS_ONLY=1` and alice during `rgs-soak`, rapid gossip sync alone, so
setup also measures how long the RGS server takes to publish lampo's
channels.

```bash
./signet-sim.sh lampo
```
