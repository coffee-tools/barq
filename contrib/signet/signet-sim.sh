#!/usr/bin/env bash
# barq on Mutinynet, end to end: alice (barq) -> carol (router) -> bob (payee).
#
#   signet-sim.sh              wait for funds, open channels, run every check
#   signet-sim.sh checks       only the payment checks (channels already open)
#   signet-sim.sh public       the checks over the faucet's public node
#   signet-sim.sh rgs          rapid gossip sync: alice routes and pays with
#                              the Mutinynet RGS snapshots only
#   signet-sim.sh rgs-soak H   H hours (default 6) of RGS-only syncs, routes
#                              and payments every 10 minutes
#
# alice and carol must hold Mutinynet coins (faucet.mutinynet.com). Each check
# prints PASS or FAIL; the summary and exit status say whether barq paid,
# failed and recorded payments as expected on the real signet.
#
# Env: CHANNEL_SAT (50000), MIN_FUNDS_SAT (60000), ROOT (~/barq-signet)
set -uo pipefail

ROOT=${ROOT:-$HOME/barq-signet}
NODES=$ROOT/nodes.sh
CHANNEL_SAT=${CHANNEL_SAT:-50000}
MIN_FUNDS_SAT=${MIN_FUNDS_SAT:-60000}
LOG=$ROOT/signet-sim.log
PASS=0; FAIL=0; FAILED=()

say() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$LOG"; }
cli() { "$NODES" cli "$@"; }
jq_() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)"; }
pass() { PASS=$((PASS + 1)); say "PASS $1"; }
fail() { FAIL=$((FAIL + 1)); FAILED+=("$1"); say "FAIL $1: ${2:-}"; }
# check NAME CMD...: CMD's exit status decides.
# A passing check's last output line, if any, goes to the log with it.
check() { local name=$1; shift; local out last; if out=$("$@" 2>&1); then last=$(echo "$out" | tail -1 | cut -c1-300); pass "$name${last:+ ($last)}"; else fail "$name" "$(echo "$out" | tail -3 | tr '\n' ' ' | cut -c1-300)"; fi; }
id() { cli "$1" getinfo | jq_ 'd["id"]'; }
onchain_sat() { cli "$1" listfunds | jq_ 'sum(o["amount_msat"] for o in d["outputs"] if o["status"]=="confirmed")//1000'; }

wait_for() { # SECONDS DESCRIPTION CMD...
  local limit=$1 what=$2; shift 2
  local start=$SECONDS
  until "$@" >/dev/null 2>&1; do
    [ $((SECONDS - start)) -gt "$limit" ] && { say "timed out after ${limit}s waiting for $what"; return 1; }
    sleep 15
  done
}

has_funds() { [ "$(onchain_sat alice)" -ge "$MIN_FUNDS_SAT" ] && [ "$(onchain_sat carol)" -ge "$MIN_FUNDS_SAT" ]; }
channel_normal() { # FROM TO
  cli "$1" listpeerchannels "$(id "$2")" | jq_ '"ok" if any(c["state"]=="CHANNELD_NORMAL" for c in d["channels"]) else ""' | grep -q ok
}
# alice's barq knows both channels, so it can route alice -> carol -> bob.
barq_routes_to_bob() {
  cli alice barqsyncgossip >/dev/null 2>&1
  cli alice -k barqrouteinfo dest_pubkey="$(id bob)" amount_msat=10000 | jq_ 'len(d["route"])' | grep -q '^2$'
}

setup() {
  say "== setup: alice $(id alice) carol $(id carol) bob $(id bob)"
  wait_for 86400 "faucet coins on alice and carol (>= $MIN_FUNDS_SAT sat each)" has_funds || exit 1
  say "funds: alice $(onchain_sat alice) sat, carol $(onchain_sat carol) sat"
  local carol_addr bob_addr
  carol_addr="$(id carol)@127.0.0.1:19848"; bob_addr="$(id bob)@127.0.0.1:19847"
  cli alice connect "$carol_addr" >/dev/null
  cli carol connect "$bob_addr" >/dev/null
  channel_normal alice carol || cli alice fundchannel "$(id carol)" "$CHANNEL_SAT" | tee -a "$LOG"
  channel_normal carol bob || cli carol fundchannel "$(id bob)" "$CHANNEL_SAT" | tee -a "$LOG"
  # 3 confirmations at 30s blocks, then 6 for the announcements.
  wait_for 1800 "alice -> carol CHANNELD_NORMAL" channel_normal alice carol || exit 1
  wait_for 1800 "carol -> bob CHANNELD_NORMAL" channel_normal carol bob || exit 1
  wait_for 3600 "barq to route alice -> carol -> bob" barq_routes_to_bob || exit 1
  say "channels ready"
}

# --- the checks ----------------------------------------------------------

expect_error() { # CODE NODE ARGS...
  local code=$1; shift; local r
  if r=$(cli "$@" 2>&1); then echo "unexpectedly succeeded: $r"; return 1; fi
  echo "$r" | grep -q "\"code\": $code" || { echo "wrong error: $r"; return 1; }
}

bolt11_barqpay() {
  local inv; inv=$(cli bob invoice 20000 "b11-$RANDOM" barq | jq_ 'd["bolt11"]')
  local r; r=$(cli alice -k barqpay invoice="$inv") || { echo "$r"; return 1; }
  echo "$r" | jq_ '"ok" if d["status"]=="complete" and len(d["route"])==2 and d["invoice_type"]=="bolt11" else d' | grep -q ok
}
bolt11_rpay() {
  local inv; inv=$(cli bob invoice 15000 "rpay-$RANDOM" barq | jq_ 'd["bolt11"]')
  local r; r=$(cli alice rpay "$inv") || { echo "$r"; return 1; }
  echo "$r" | jq_ '"ok" if d["status"]=="complete" and d["parts"]>=1 and d["destination"] else d' | grep -q ok
}
bolt12_pay() {
  local offer inv r
  offer=$(cli bob offer 12000msat "barq bolt12 $RANDOM" | jq_ 'd["bolt12"]')
  inv=$(cli alice fetchinvoice "$offer" | jq_ 'd["invoice"]') || return 1
  r=$(cli alice rpay "$inv") || { echo "$r"; return 1; }
  echo "$r" | jq_ '"ok" if d["status"]=="complete" else d' | grep -q ok
}
bolt12_any_amount() {
  local offer inv r
  offer=$(cli bob offer any "barq bolt12 any $RANDOM" | jq_ 'd["bolt12"]')
  inv=$(cli alice fetchinvoice "$offer" 7000msat | jq_ 'd["invoice"]') || return 1
  r=$(cli alice -k barqpay invoice="$inv") || { echo "$r"; return 1; }
  echo "$r" | jq_ '"ok" if d["status"]=="complete" and d["invoice_type"]=="bolt12" and d["blinded_hops"]>=1 else d' | grep -q ok
}
bolt11_unknown_invoice() {
  local label="gone-$RANDOM" inv
  inv=$(cli bob invoice 10000 "$label" gone | jq_ 'd["bolt11"]')
  cli bob delinvoice "$label" unpaid >/dev/null
  expect_error 203 alice rpay "$inv"
}
bolt12_paid_twice_by_other() {
  # Single-use offer: once paid, a second invoice for it fails at bob.
  local offer i1 i2
  offer=$(cli bob -k offer amount=9000msat description="single $RANDOM" single_use=true | jq_ 'd["bolt12"]') || return 1
  i1=$(cli alice fetchinvoice "$offer" | jq_ 'd["invoice"]')
  i2=$(cli alice fetchinvoice "$offer" | jq_ 'd["invoice"]')
  cli alice rpay "$i1" >/dev/null || return 1
  expect_error 203 alice rpay "$i2"
}
too_large() {
  local inv; inv=$(cli bob invoice $((CHANNEL_SAT * 2000)) "big-$RANDOM" big | jq_ 'd["bolt11"]')
  expect_error 205 alice rpay "$inv"
}
fee_limit() {
  local inv; inv=$(cli bob invoice 10000 "fee-$RANDOM" fee | jq_ 'd["bolt11"]')
  expect_error 206 alice -k rpay bolt11="$inv" maxfee=0msat
}
self_pay() {
  local inv; inv=$(cli alice invoice 5000 "self-$RANDOM" self | jq_ 'd["bolt11"]')
  cli alice rpay "$inv" | jq_ 'd["status"]' | grep -q complete
}
already_paid() {
  local inv a b
  inv=$(cli bob invoice 8000 "twice-$RANDOM" twice | jq_ 'd["bolt11"]')
  a=$(cli alice rpay "$inv" | jq_ 'd["payment_preimage"]') || return 1
  b=$(cli alice rpay "$inv" | jq_ 'd["payment_preimage"]') || return 1
  [ "$a" = "$b" ] || return 1
  # One payment in listsendpays, as part 1 of group 1, like pay makes it.
  cli alice listsendpays "$inv" | jq_ '"ok" if [(p["groupid"], p.get("partid")) for p in d["payments"]]==[(1, 1)] else d' | grep -q ok
}
concurrent_payments() {
  # Two payments in flight at once, and barq still answers other commands.
  local a b out1 out2 t0 t_inspect p1 p2
  a=$(cli bob invoice 6000 "conc-a-$RANDOM" conc | jq_ 'd["bolt11"]')
  b=$(cli bob invoice 7000 "conc-b-$RANDOM" conc | jq_ 'd["bolt11"]')
  out1=$(mktemp); out2=$(mktemp)
  t0=$SECONDS
  cli alice rpay "$a" > "$out1" 2>&1 & p1=$!
  cli alice rpay "$b" > "$out2" 2>&1 & p2=$!
  sleep 1
  cli alice barqinspect >/dev/null || { echo "barqinspect failed during payments"; return 1; }
  t_inspect=$((SECONDS - t0))
  wait $p1 $p2
  local t_done=$((SECONDS - t0))
  for f in "$out1" "$out2"; do
    jq_ 'd["status"]' < "$f" | grep -q complete || { echo "payment failed: $(head -c 300 "$f")"; return 1; }
  done
  rm -f "$out1" "$out2"
  echo "barqinspect after ${t_inspect}s, both payments after ${t_done}s"
  # On a slow chain the payments take seconds: barqinspect must not wait for them.
  [ "$t_inspect" -lt "$t_done" ] || [ "$t_done" -le 2 ]
}
history() {
  cli alice barqinspect | jq_ '"ok" if len(d["payments"])>=5 and any(p["status"]=="complete" for p in d["payments"]) and any(p["status"]=="failed" for p in d["payments"]) else d' | grep -q ok
}
pay_takeover() {
  # Restart alice with barq serving `pay` (xpay's takeover off).
  # The daemonized lightningd must not inherit check's output pipe, or
  # check's command substitution never sees EOF.
  "$NODES" stop alice >/dev/null 2>&1 < /dev/null; sleep 5
  BARQ_OVERRIDE_PAY=1 "$NODES" start alice >/dev/null 2>&1 < /dev/null || return 1
  sleep 10
  cli alice connect "$(id carol)@127.0.0.1:19848" >/dev/null
  wait_for 600 "alice -> carol reconnected" channel_normal alice carol || return 1
  local inv; inv=$(cli bob invoice 11000 "takeover-$RANDOM" takeover | jq_ 'd["bolt11"]')
  cli alice pay "$inv" | jq_ 'd["status"]' | grep -q complete || return 1
  grep -q "serving \`pay\` with barq" "$ROOT/nodes/alice/log"
}

checks() {
  say "== checks"
  check "BOLT11 barqpay over two hops" bolt11_barqpay
  check "BOLT11 rpay in pay's format" bolt11_rpay
  check "BOLT12 rpay of a fixed-amount offer" bolt12_pay
  check "BOLT12 barqpay of an any-amount offer" bolt12_any_amount
  check "BOLT11 unknown invoice fails with 203" bolt11_unknown_invoice
  check "BOLT12 used single-use offer fails with 203" bolt12_paid_twice_by_other
  check "unroutable amount fails with 205" too_large
  check "fee limit fails with 206" fee_limit
  check "self-payment completes" self_pay
  check "already paid invoice answers with the same payment, grouped like pay" already_paid
  check "two payments at once while barq answers other commands" concurrent_payments
  check "barqinspect records successes and failures" history
  check "pay is served by barq" pay_takeover
  say "== $PASS passed, $FAIL failed${FAILED:+: ${FAILED[*]}}"
  [ "$FAIL" -eq 0 ]
}


# --- the public network: alice and bob also have channels with the
# faucet's LND node, so bob is reachable over two independent paths.
FAUCET_NODE=02465ed5be53d04fde66c9418ff14a5f2267723810176c9212b722e542dc1afb1b
FAUCET_ADDR=$FAUCET_NODE@45.79.52.207:9735
MUTINYNET_CLI=${MUTINYNET_CLI:-$HOME/.cargo/bin/mutinynet-cli}

channel_normal_with() { # NODE PUBKEY
  cli "$1" listpeerchannels "$2" | jq_ '"ok" if any(c["state"]=="CHANNELD_NORMAL" for c in d["channels"]) else ""' | grep -q ok
}
# alice's gossip knows faucet -> bob, so barq has a second path to bob.
barq_knows_faucet_bob() {
  cli alice barqsyncgossip >/dev/null 2>&1
  cli alice -k listchannels source="$FAUCET_NODE" | jq_ '"ok" if any(c["destination"]=="'"$(id bob)"'" for c in d["channels"]) else ""' | grep -q ok
}

setup_public() {
  say "== setup: public network through the faucet node"
  cli alice connect "$FAUCET_ADDR" >/dev/null
  cli bob connect "$FAUCET_ADDR" >/dev/null
  wait_for 3600 "faucet -> alice CHANNELD_NORMAL" channel_normal_with alice "$FAUCET_NODE" || exit 1
  wait_for 3600 "faucet -> bob CHANNELD_NORMAL" channel_normal_with bob "$FAUCET_NODE" || exit 1
  wait_for 3600 "alice's gossip to learn faucet -> bob" barq_knows_faucet_bob || exit 1
  say "public channels ready"
}

pay_public_node() {
  # An invoice of the faucet's own node, paid over the public channel.
  local inv r
  inv=$("$MUTINYNET_CLI" bolt11 5 2>&1 | tail -1)
  case "$inv" in lntbs*) ;; *) echo "no faucet invoice: $inv"; return 1 ;; esac
  r=$(cli alice -k barqpay invoice="$inv") || { echo "$r"; return 1; }
  echo "$r" | jq_ '"ok" if d["status"]=="complete" and d["destination"]=="'"$FAUCET_NODE"'" else d' | grep -q ok
}
fallback_to_second_path() {
  # Drain carol -> bob, then pay bob a small amount: fees then decide, the
  # cheap path through carol fails at carol, and barq completes through the
  # faucet in the same call. (For large amounts barq's scorer already
  # prefers the faucet's bigger channels.)
  local spend inv r
  spend=$(cli carol listpeerchannels "$(id bob)" | jq_ 'max(c["spendable_msat"] for c in d["channels"])')
  spend=$(( spend - 500000 ))
  if [ "$spend" -gt 0 ]; then
    inv=$(cli bob invoice "$spend" "drain-$RANDOM" drain | jq_ 'd["bolt11"]')
    cli carol pay "$inv" >/dev/null || { echo "could not drain carol -> bob"; return 1; }
  fi
  inv=$(cli bob invoice 2000000 "fallback-$RANDOM" fallback | jq_ 'd["bolt11"]')
  r=$(cli alice -k barqpay invoice="$inv") || { echo "$r"; return 1; }
  local verdict
  # Either barq falls back in this call, or its scorer remembers that
  # carol -> bob failed before and goes through the faucet at once.
  verdict=$(echo "$r" | jq_ '"ok" if d["status"]=="complete" and d["route"][0]["id"]=="'"$FAUCET_NODE"'" and (len(d["attempts"])>=2 and d["attempts"][0]["status"]=="failed" or len(d["attempts"])==1) else [(a["status"], [h["channel"] for h in a["route"]], a.get("error")) for a in d["attempts"]]')
  [ "$verdict" = ok ] || { echo "$verdict"; return 1; }
}

checks_public() {
  say "== public network checks"
  check "pay the faucet node over the public channel" pay_public_node
  check "fallback to the second path after a failure mid-route" fallback_to_second_path
  say "== $PASS passed, $FAIL failed${FAILED:+: ${FAILED[*]}}"
  [ "$FAIL" -eq 0 ]
}


# --- rapid gossip sync: alice's barq without Core Lightning's gossip, so
# every channel it routes over beyond its own comes from the RGS snapshots.
ALICE_LOG=$ROOT/nodes/alice/log
BARQ_DATA=$ROOT/nodes/alice/signet/barq
RGS_SAMPLE=${RGS_SAMPLE:-40}

# restart_alice [VAR=VALUE...]: restart alice with these nodes.sh settings.
# The daemonized lightningd must not inherit check's output pipe.
restart_alice() {
  "$NODES" stop alice >/dev/null 2>&1 < /dev/null
  local i; for i in $(seq 60); do cli alice getinfo >/dev/null 2>&1 || break; sleep 1; done
  env "$@" "$NODES" start alice >/dev/null 2>&1 < /dev/null || return 1
  cli alice connect "$(id carol)@127.0.0.1:19848" >/dev/null 2>&1
  cli alice connect "$FAUCET_ADDR" >/dev/null 2>&1
  wait_for 600 "alice -> carol reconnected" channel_normal alice carol
}
rgs_only() { restart_alice BARQ_CLN_GOSSIP=false "$@"; }
# The rapid gossip sync lines of alice's log since barq last started.
rgs_log() {
  python3 - "$ALICE_LOG" <<'EOF'
import sys
lines = open(sys.argv[1], errors="replace").read().splitlines()
# barq loads its graph right before it logs that it is initialized.
start = max([i for i, l in enumerate(lines) if "barq initialized on" in l] or [0])
loads = [i for i, l in enumerate(lines[:start]) if "loaded network graph" in l and start - i < 200]
start = loads[-1] if loads else start
for l in lines[start:]:
    if "rapid gossip sync" in l or "loaded network graph" in l:
        print(l)
EOF
}
rgs_synced() { # alice's barq applied a snapshot since it started
  rgs_log | grep -q "rapid gossip sync: applied"
}

# The channel announcements in the server's full snapshot.
rgs_snapshot_channels() {
  curl -s --max-time 60 "${RGS:-https://rgs.mutinynet.com/snapshot}/0" | python3 -c '
import struct, sys
b = sys.stdin.buffer.read()
nodes, = struct.unpack(">I", b[40:44])
o = 44 + 33 * nodes
print(struct.unpack(">I", b[o:o + 4])[0])
'
}
rgs_full_sync() {
  # From an empty graph: barq holds every channel of the server's full
  # snapshot (the server may know fewer than Core Lightning's gossip).
  local r ours cln snap
  r=$(cli alice barqsyncgossip) || { echo "$r"; return 1; }
  cln=$(cli alice listchannels | jq_ 'len({c["short_channel_id"] for c in d["channels"]})')
  snap=$(rgs_snapshot_channels) || return 1
  ours=$(echo "$r" | jq_ 'd["channels"]')
  echo "$r" | jq_ '"ok" if not d["gossip_store"]["enabled"] and d["rapid_gossip_sync"].get("last_sync_timestamp", 0) > '"$(( $(date +%s) - 14 * 86400 ))"' else d' | grep -q ok || return 1
  [ "$ours" -ge "$snap" ] || { echo "barq holds $ours channels, the snapshot announces $snap"; return 1; }
  echo "barq holds $ours channels, the server's snapshot announces $snap, Core Lightning's gossip knows $cln"
}
rgs_next_fetch_continues() {
  # The next sync asks for the snapshot since the last one applied.
  cli alice barqsyncgossip >/dev/null || return 1
  rgs_log | python3 -c '
import re, sys
lines = sys.stdin.read().splitlines()
fetches = [i for i, l in enumerate(lines) if "rapid gossip sync: fetching" in l]
applied = [(i, int(re.search(r"new timestamp (\d+)", l).group(1))) for i, l in enumerate(lines) if "new timestamp" in l]
last = fetches[-1]
before = [ts for i, ts in applied if i < last]
fetched = int(re.search(r"/(\d+)\s*$", lines[last]).group(1))
if not before or fetched != before[-1]:
    sys.exit(f"fetched from {fetched} after applying {before[-3:]}")
print(f"fetched from {fetched}, the last timestamp applied")
'
}
rgs_restart_continues() {
  # After a restart barq loads the graph and continues from its timestamp.
  local before; before=$(cli alice barqsyncgossip | jq_ 'd["rapid_gossip_sync"]["last_sync_timestamp"]') || return 1
  rgs_only || return 1
  wait_for 300 "the first sync after the restart" rgs_synced || return 1
  rgs_log | python3 -c '
import re, sys
before = int(sys.argv[1])
lines = sys.stdin.read().splitlines()
loaded = [l for l in lines if "loaded network graph" in l]
fetch = next(l for l in lines if "rapid gossip sync: fetching" in l)
fetched = int(re.search(r"/(\d+)\s*$", fetch).group(1))
if not loaded or f"Some({before})" not in loaded[-1] or fetched != before:
    sys.exit(f"before {before}, loaded {loaded[-1:]}, fetched from {fetched}")
print(f"continued from {before}")
' "$before"
}
rgs_routes_like_cln() {
  # Route to a sample of public nodes with barq (RGS only) and with Core
  # Lightning (its own gossip): barq must find most of the routes Core
  # Lightning finds, over channels Core Lightning knows too.
  python3 - "$NODES" "$RGS_SAMPLE" "$(id alice)" <<'EOF'
import json, random, subprocess, sys
nodes, sample, alice = sys.argv[1], int(sys.argv[2]), sys.argv[3]
def cli(*args):
    p = subprocess.run([nodes, "cli", "alice", *args], capture_output=True, text=True)
    return json.loads(p.stdout) if p.returncode == 0 else None
channels = cli("listchannels")["channels"]
known = {c["short_channel_id"] for c in channels}
dests = sorted({c["destination"] for c in channels if c["active"]} - {alice})
random.seed()
dests = random.sample(dests, min(sample, len(dests)))
cln_ok = barq_ok = both = 0
unknown_hops = []
missed = []
for dest in dests:
    cln = cli("getroute", dest, "10000", "1")
    barq = cli("-k", "barqrouteinfo", f"dest_pubkey={dest}", "amount_msat=10000")
    cln_ok += cln is not None
    barq_ok += barq is not None
    both += cln is not None and barq is not None
    if cln is not None and barq is None:
        missed.append(dest[:16])
    if barq is not None:
        unknown_hops += [h["channel"] for h in barq["route"][1:] if h["channel"] not in known]
print(f"{len(dests)} nodes: Core Lightning routes to {cln_ok}, barq to {barq_ok}, both {both}; "
      f"barq missed {missed[:5]}; hops Core Lightning does not know: {unknown_hops[:5]}")
if cln_ok and both * 10 < cln_ok * 8:
    sys.exit("barq found fewer than 80% of Core Lightning's routes")
if len(unknown_hops) > 2:
    sys.exit("barq routed over channels Core Lightning does not know")
EOF
}
rgs_server_down() {
  # An unreachable RGS server: syncs report it, payments still route with
  # the gossip_store.
  restart_alice RGS=http://127.0.0.1:9/snapshot || return 1
  cli alice barqsyncgossip | jq_ '"ok" if "error" in d["rapid_gossip_sync"] and d["gossip_store"]["enabled"] else d' | grep -q ok || return 1
  bolt11_barqpay
}
rgs_wrong_chain() {
  # A server for another chain: its snapshots are refused, the graph is kept.
  restart_alice RGS=https://rapidsync.lightningdevkit.org/snapshot BARQ_CLN_GOSSIP=false || return 1
  local r; r=$(cli alice barqsyncgossip) || { echo "$r"; return 1; }
  echo "$r" | jq_ '"ok" if "chain hash" in d["rapid_gossip_sync"].get("error", "") and d["channels"] > 100 else d' | grep -q ok || { echo "$r"; return 1; }
  bolt11_barqpay
}
rgs_garbage() {
  # A server answering something that is not a snapshot.
  restart_alice RGS=https://faucet.mutinynet.com BARQ_CLN_GOSSIP=false || return 1
  local r; r=$(cli alice barqsyncgossip) || { echo "$r"; return 1; }
  echo "$r" | jq_ '"ok" if "error" in d["rapid_gossip_sync"] and d["channels"] > 100 else d' | grep -q ok || { echo "$r"; return 1; }
  cli alice getinfo >/dev/null
}

setup_rgs() {
  say "== setup: alice on rapid gossip sync only, from an empty graph"
  "$NODES" stop alice >/dev/null 2>&1 < /dev/null; sleep 5
  rm -f "$BARQ_DATA"/network_graph.*.bin "$BARQ_DATA"/scorer.*.bin
  rgs_only || exit 1
  wait_for 600 "the first rapid gossip sync" rgs_synced || exit 1
  say "synced: $(rgs_log | grep applied | tail -1 | sed 's/.*rapid gossip sync: //')"
}
checks_rgs() {
  say "== rapid gossip sync checks"
  check "RGS-only full sync from an empty graph" rgs_full_sync
  check "the next sync continues from the last timestamp" rgs_next_fetch_continues
  check "RGS-only routes match Core Lightning's" rgs_routes_like_cln
  check "BOLT11 over two hops learned from RGS" bolt11_barqpay
  check "BOLT12 over two hops learned from RGS" bolt12_pay
  check "BOLT12 any-amount offer learned from RGS" bolt12_any_amount
  check "unroutable amount fails with 205 on RGS" too_large
  check "two payments at once on RGS" concurrent_payments
  check "pay the faucet node on RGS" pay_public_node
  check "a restart continues from the persisted timestamp" rgs_restart_continues
  check "an unreachable RGS server falls back to the gossip_store" rgs_server_down
  check "another chain's snapshots are refused" rgs_wrong_chain
  check "a server answering garbage is reported" rgs_garbage
  say "restoring alice's configuration"
  restart_alice || say "alice did not restart"
  say "== $PASS passed, $FAIL failed${FAILED:+: ${FAILED[*]}}"
  [ "$FAIL" -eq 0 ]
}

rgs_soak() { # HOURS
  # Leave alice on RGS only, syncing every 10 minutes, and keep checking
  # that it advances, routes like Core Lightning and pays.
  local hours=${1:-6} round=0 end last=0 ts
  end=$((SECONDS + hours * 3600))
  say "== rapid gossip sync soak for ${hours}h"
  rgs_only BARQ_SYNC_INTERVAL=600 || exit 1
  wait_for 600 "the first rapid gossip sync" rgs_synced || exit 1
  while [ $SECONDS -lt $end ]; do
    round=$((round + 1))
    ts=$(cli alice barqsyncgossip | jq_ 'd["rapid_gossip_sync"].get("last_sync_timestamp", 0)')
    if [ "${ts:-0}" -ge "$last" ]; then pass "round $round: timestamp $ts"; else fail "round $round" "timestamp went back from $last to $ts"; fi
    last=${ts:-0}
    check "round $round: next sync continues" rgs_next_fetch_continues
    RGS_SAMPLE=15 check "round $round: routes like Core Lightning" rgs_routes_like_cln
    check "round $round: BOLT11 payment" bolt11_barqpay
    check "round $round: BOLT12 payment" bolt12_pay
    sleep 600
  done
  say "restoring alice's configuration"
  restart_alice || say "alice did not restart"
  say "== $PASS passed, $FAIL failed${FAILED:+: ${FAILED[*]}}"
  [ "$FAIL" -eq 0 ]
}

case "${1:-all}" in
  all) setup && checks ;;
  checks) checks ;;
  setup) setup ;;
  public) setup_public && checks_public ;;
  rgs) setup_rgs && checks_rgs ;;
  rgs-soak) rgs_soak "${2:-6}" ;;
  *) echo "usage: $0 [all|setup|checks|public|rgs|rgs-soak [HOURS]]"; exit 1 ;;
esac
