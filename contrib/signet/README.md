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
