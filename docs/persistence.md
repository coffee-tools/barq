# What barq persists, and why

Issue #7 asked two questions. Which information about past payments should
barq store? Which information about the network helps it route better?
This document answers both and says where each piece comes from.

Everything lives under `<lightning-dir>/<network>/barq/`.

## Past payments

Each `barqpay` call is one record, `payments/<id>.json`. The `id` is
returned by `barqpay` and increases forever. Records are written
atomically, so a crash never leaves half a file. They are created as soon
as the invoice is decoded and the amount is known, and rewritten when the
payment ends.

| Field | Why it is stored | Source |
| --- | --- | --- |
| `payment_hash`, `invoice`, `invoice_type`, `destination` | Identify the payment and look it up later. The invoice string is kept so the payment can be retried or audited. | `decode` |
| `amount_msat`, `amount_sent_msat`, `fee_msat` | What was paid and what it cost. This is the basis of any fee report. | Invoice or request; `waitsendpay` |
| `status`, `payment_preimage`, `completed_at`, `error` | The outcome. The preimage is the proof of payment. | `waitsendpay` |
| `label`, `created_at` | The caller's own reference, and timing. | Request |
| `attempts[]` | Every route tried: strategy, hops (node, channel, fee, CLTV), fee, outcome, the error, and the channel that failed. This is what tells you why a payment cost what it did or why it failed. | The strategy's route; `waitsendpay` errors mapped onto the route |
| `events[]` | A journal of what barq did for this payment: ranking, rejected strategies, each attempt, exclusions, the outcome. These are the payment's logs, kept per payment instead of mixed into the node log. | barq |

Core Lightning keeps its own view in `listsendpays`. barq records what Core
Lightning cannot know: the strategy that chose each route, the routes that
were rejected, and how a failure position maps onto the route. The payment
hash and `partid`/`groupid` link a record to `listsendpays`.

Inspect records with `barqinspect` (issue #8):

- `barqinspect id=N` returns one payment.
- `barqinspect payment_hash=H` returns every attempt to pay that hash.
- `barqinspect` lists the most recent payments; `limit` sets how many.

## The network

| File | Contents | Why |
| --- | --- | --- |
| `network_graph.<network>.bin` | LDK's network graph, including the last rapid gossip sync timestamp | Routing works right after a restart. The next sync downloads only the delta. |
| `scorer.<network>.bin` | LDK's probabilistic scorer: liquidity bounds learned from every success and failure | Routes improve with every payment and keep improving across restarts. This is the network information that matters most for routing, and Core Lightning does not keep it. |

The graph comes from rapid gossip sync and the Core Lightning
`gossip_store`. The scorer is updated from each payment's result. Channel
balances are not stored: they change all the time, and
`listpeerchannels` is asked for them before every payment.

## Not stored

- **Node log lines.** They stay in Core Lightning's log. The per-payment
  journal holds what matters for one payment.
- **Raw onions and shared secrets.** They are only useful for the attempt
  in flight, and they are sensitive.
- **A database.** One JSON file per payment needs no dependency, can be
  read with any tool, and is enough for the volume of an experimental
  node. If volume grows, the store sits behind one small type
  (`barq_common::history::PaymentStore`) and can move to SQLite.
