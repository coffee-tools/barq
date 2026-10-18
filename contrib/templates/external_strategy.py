#!/usr/bin/env python3
"""__NAME__: an external barq routing strategy.

barq runs this program for every route it needs (see "External strategies"
in USER_GUIDE.md). It reads one JSON request on stdin and must print one
JSON response on stdout:

  request:  {"version": 1, "source": "<node id>", "destination": "<node id>",
             "amount_msat": 1000, "final_cltv_delta": 18,
             "first_hops": [{"peer_id", "short_channel_id", "spendable_msat",
                             "htlc_minimum_msat", "htlc_maximum_msat"}],
             "excluded_channels": ["103x1x0"], "rpc_file": "lightning-rpc"}
  response: {"route": [{"id", "channel", "fee_msat", "cltv_expiry_delta"}]}
            or {"error": "why there is no route"}

A hop's `fee_msat` is what that hop's node receives on top of the amount it
forwards (the last hop's is the amount delivered), and `cltv_expiry_delta`
is the delta it adds (the last hop's is the final CLTV delta), exactly like
an LDK route. The working directory is the node's lightning-dir, so
`rpc_file` is a path to Core Lightning's JSON-RPC socket.

This template finds the path with the fewest hops over `listchannels`;
replace `find_path` with your own algorithm.
"""
import json
import socket
import sys
from collections import deque


def rpc(rpc_file, method, params=None):
    """Minimal Core Lightning JSON-RPC client (standard library only)."""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.connect(rpc_file)
        request = {"jsonrpc": "2.0", "id": 1, "method": method, "params": params or {}}
        sock.sendall(json.dumps(request).encode())
        buffer = b""
        decoder = json.JSONDecoder()
        while True:
            chunk = sock.recv(65536)
            if not chunk:
                raise RuntimeError(f"{method}: connection closed")
            buffer += chunk
            try:
                response, _ = decoder.raw_decode(buffer.decode())
                break
            except ValueError:
                continue
    if "error" in response:
        raise RuntimeError(f"{method}: {response['error']}")
    return response["result"]


def find_path(request, channels):
    """Fewest-hops path from source to destination, as a list of
    (node, channel, policy) where policy is the channel_update of the
    direction used. Our own first hop uses the channels in first_hops."""
    source, destination = request["source"], request["destination"]
    amount = request["amount_msat"]
    excluded = set(request["excluded_channels"])

    graph = {}
    for channel in channels:
        if channel["short_channel_id"] in excluded or not channel["active"]:
            continue
        graph.setdefault(channel["source"], []).append(channel)

    # Our own channels: only the ones that can carry the amount.
    first = {
        hop["short_channel_id"]
        for hop in request["first_hops"]
        if hop["spendable_msat"] >= amount and hop["short_channel_id"] not in excluded
    }

    previous = {source: None}
    queue = deque([source])
    while queue:
        node = queue.popleft()
        if node == destination:
            break
        for channel in graph.get(node, []):
            peer = channel["destination"]
            if peer in previous:
                continue
            if node == source and channel["short_channel_id"] not in first:
                continue
            previous[peer] = (node, channel)
            queue.append(peer)
    if destination not in previous:
        return None

    path = []
    node = destination
    while previous[node] is not None:
        parent, channel = previous[node]
        path.append((node, channel))
        node = parent
    return list(reversed(path))


def build_route(request, path):
    """Turn a path into barq's route format, computing fees and CLTV deltas
    from the destination back to us."""
    hops = [{"id": node, "channel": channel["short_channel_id"]} for node, channel in path]
    hops[-1]["fee_msat"] = request["amount_msat"]
    hops[-1]["cltv_expiry_delta"] = request["final_cltv_delta"]
    amount = request["amount_msat"]
    # Hop i is paid by the channel of hop i+1, whose source is node i.
    for i in range(len(path) - 2, -1, -1):
        policy = path[i + 1][1]
        fee = policy["base_fee_millisatoshi"] + amount * policy["fee_per_millionth"] // 1_000_000
        hops[i]["fee_msat"] = fee
        hops[i]["cltv_expiry_delta"] = policy["delay"]
        amount += fee
    return hops


def main():
    request = json.load(sys.stdin)
    if request.get("version") != 1:
        print(json.dumps({"error": f"unsupported request version {request.get('version')}"}))
        return
    channels = rpc(request["rpc_file"], "listchannels")["channels"]
    path = find_path(request, channels)
    if not path:
        print(json.dumps({"error": "__NAME__: no path to the destination"}))
        return
    print(json.dumps({"route": build_route(request, path)}))


if __name__ == "__main__":
    main()
