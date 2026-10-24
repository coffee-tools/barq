#!/usr/bin/env bash
# barq signet (Mutinynet) test nodes: alice (barq payer), bob (payee),
# carol (router). Everything lives under ~/barq-signet and only talks to the
# existing Mutinynet bitcoind at 127.0.0.1:38332. Nothing else is touched.
#
#   nodes.sh start [node...]   start nodes (default: all)
#   nodes.sh stop [node...]    stop nodes
#   nodes.sh cli NODE ARGS...  lightning-cli for NODE
#   nodes.sh status            getinfo summary of every node
set -uo pipefail

ROOT=$HOME/barq-signet
CLN=$ROOT/cln/usr
BARQ=$ROOT/barq/target/release/barq-plugin
NODES=(alice bob carol)
declare -A PORT=([alice]=19846 [bob]=19847 [carol]=19848)
RGS=${RGS:-https://rgs.mutinynet.com/snapshot}
export PATH=$CLN/bin:$PATH

dir() { echo "$ROOT/nodes/$1"; }

write_config() {
  local n=$1 d; d=$(dir "$n")
  mkdir -p "$d"
  cat > "$d/config" <<EOF
network=signet
bitcoin-rpcconnect=127.0.0.1
bitcoin-rpcport=38332
bitcoin-rpcuser=testutil
bitcoin-rpcpassword=testutilpassword
addr=127.0.0.1:${PORT[$n]}
alias=barq-$n
log-file=$d/log
log-level=debug
# The tests do not use gRPC; several nodes would fight over its port.
disable-plugin=cln-grpc
EOF
  if [ "$n" = alice ]; then
    cat >> "$d/config" <<EOF
plugin=$BARQ
barq-rgs-url=$RGS
barq-sync-interval=600
EOF
    if [ "${BARQ_OVERRIDE_PAY:-0}" = 1 ]; then
      # xpay also takes over `pay`: switch it off so barq serves it.
      printf 'barq-override-pay=true\nxpay-handle-pay=false\n' >> "$d/config"
    fi
    [ -n "${BARQ_EXTERNAL:-}" ] && echo "barq-external-strategy=$BARQ_EXTERNAL" >> "$d/config"
  fi
}

cli() { local n=$1; shift; lightning-cli --lightning-dir="$(dir "$n")" "$@"; }

start() {
  for n in "${@:-${NODES[@]}}"; do
    if cli "$n" getinfo >/dev/null 2>&1; then echo "$n already running"; continue; fi
    write_config "$n"
    # The Mutinynet bitcoind sometimes drops RPC connections, and
    # lightningd exits when an important plugin's first bitcoind call
    # fails: try a few times.
    for try in 1 2 3 4 5; do
      lightningd --lightning-dir="$(dir "$n")" --daemon || { echo "$n failed to start"; tail -20 "$(dir "$n")/log"; break; }
      for _ in $(seq 60); do cli "$n" getinfo >/dev/null 2>&1 && break 2; sleep 1; done
      echo "$n did not come up (try $try): $(grep BROKEN "$(dir "$n")/log" | tail -1 | cut -c1-160)"
    done
    echo "$n started: $(cli "$n" getinfo | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d["id"], d["blockheight"])')"
  done
}

stop() { for n in "${@:-${NODES[@]}}"; do cli "$n" stop >/dev/null 2>&1 && echo "$n stopped"; done; }

status() {
  for n in "${NODES[@]}"; do
    cli "$n" getinfo 2>/dev/null | python3 2>/dev/null -c "import json,sys;d=json.load(sys.stdin);print('$n', d['id'], 'height', d['blockheight'], 'peers', d['num_peers'], 'active', d['num_active_channels'], 'pending', d['num_pending_channels'])" || echo "$n down"
  done
}

cmd=${1:-status}; shift || true
case "$cmd" in
  start) start "$@" ;;
  stop) stop "$@" ;;
  cli) cli "$@" ;;
  status) status ;;
  *) echo "usage: $0 start|stop|cli|status"; exit 1 ;;
esac
