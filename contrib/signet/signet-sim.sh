#!/usr/bin/env bash
# barq on Mutinynet, end to end: alice (barq) -> carol (router) -> bob (payee).
#
#   signet-sim.sh              wait for funds, open channels, run every check
#   signet-sim.sh checks       only the payment checks (channels already open)
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
check() { local name=$1; shift; local out; if out=$("$@" 2>&1); then pass "$name"; else fail "$name" "$(echo "$out" | tail -3 | tr '\n' ' ' | cut -c1-300)"; fi; }
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
  check "barqinspect records successes and failures" history
  check "pay is served by barq" pay_takeover
  say "== $PASS passed, $FAIL failed${FAILED:+: ${FAILED[*]}}"
  [ "$FAIL" -eq 0 ]
}

case "${1:-all}" in
  all) setup && checks ;;
  checks) checks ;;
  setup) setup ;;
  *) echo "usage: $0 [all|setup|checks]"; exit 1 ;;
esac
