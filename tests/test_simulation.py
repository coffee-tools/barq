"""Route over mocked network graphs in the integration suite (issue #9).

`barq-dev-simulated-graph` loads a JSON graph of mocked channels into the
plugin's routing graph next to the real channels. The tests extend a real
two-node network with a mocked grid behind the real peer, so strategies can
be tested on topologies far bigger than what bitcoind + lightningd allow.
Payments cannot settle over mocked nodes; these tests check routing.
"""
import hashlib
import json
import os

import coincurve
import pytest

from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import only_one
from pyln.client import RpcError

barq_binary = os.path.join(os.path.dirname(__file__), "../target/debug/barq-plugin")

WIDTH = HEIGHT = 4


def mock_key(index):
    secret = hashlib.sha256(f"barq-test-mock-{index}".encode()).digest()
    return coincurve.PrivateKey(secret).public_key.format().hex()


def scid(block, tx=1, out=0):
    return f"{block}x{tx}x{out}"


def policy(base, ppm, cltv=6):
    return {"fee_base_msat": base, "fee_proportional_millionths": ppm, "cltv_expiry_delta": cltv}


def grid_spec(entry_node):
    """A WIDTH x HEIGHT grid of mocked nodes; `entry_node` (a real node)
    has a channel to grid node 0. Every grid channel charges 1 sat + 100ppm
    so fees are easy to check."""
    nodes = [mock_key(i) for i in range(WIDTH * HEIGHT)]
    channels = [{
        "short_channel_id": scid(600_000),
        "node1": entry_node,
        "node2": nodes[0],
        "capacity_sat": 5_000_000,
        "policy_1to2": policy(1000, 100),
        "policy_2to1": policy(1000, 100),
    }]
    block = 600_001
    for y in range(HEIGHT):
        for x in range(WIDTH):
            here = y * WIDTH + x
            neighbours = []
            if x + 1 < WIDTH:
                neighbours.append(here + 1)
            if y + 1 < HEIGHT:
                neighbours.append(here + WIDTH)
            for other in neighbours:
                channels.append({
                    "short_channel_id": scid(block),
                    "node1": nodes[here],
                    "node2": nodes[other],
                    "capacity_sat": 5_000_000,
                    "policy_1to2": policy(1000, 100),
                    "policy_2to1": policy(1000, 100),
                })
                block += 1
    return nodes, channels


@pytest.fixture
def simulated(node_factory, tmp_path):
    """l1 -- l2 (real), and l2 -- mocked 4x4 grid, known only to l1's barq."""
    l2 = node_factory.get_node(options={"plugin": barq_binary})
    nodes, channels = grid_spec(l2.info["id"])
    spec = tmp_path / "graph.json"
    spec.write_text(json.dumps({"channels": channels}))
    l1 = node_factory.get_node(options={
        "plugin": barq_binary,
        "barq-dev-simulated-graph": str(spec),
    })
    node_factory.join_nodes([l1, l2], wait_for_announce=True)
    return l1, l2, nodes, channels


def test_simulated_graph_is_loaded(simulated):
    l1, l2, nodes, channels = simulated
    assert l1.daemon.is_in_log(f"loaded {len(channels)} simulated channels")
    info = l1.rpc.call("barqsyncgossip", {})
    # 24 grid channels + the entry channel + the real l1 -- l2 channel.
    assert info["channels"] >= len(channels) + 1
    assert info["nodes"] >= len(nodes) + 2


def test_route_into_simulated_grid(simulated):
    """Route from l1 to the far corner of the grid: through the real peer l2,
    then 6 grid hops. Every hop must follow a channel of the spec, and the
    fee must match the mocked policies."""
    l1, l2, nodes, channels = simulated
    amount = 100_000
    corner = nodes[-1]
    info = l1.rpc.call("barqrouteinfo", {"dest_pubkey": corner, "amount_msat": amount})

    route = info["route"]
    assert route[0]["id"] == l2.info["id"]
    assert route[1]["id"] == nodes[0]
    assert route[-1]["id"] == corner
    # l2, grid node 0, then the shortest grid path: 3 right + 3 down.
    assert len(route) == 2 + (WIDTH - 1) + (HEIGHT - 1)

    # Every hop uses a channel between consecutive nodes of the spec.
    by_scid = {c["short_channel_id"]: {c["node1"], c["node2"]} for c in channels}
    previous = l2.info["id"]
    for hop in route[1:]:
        assert by_scid[hop["channel"]] == {previous, hop["id"]}, hop
        previous = hop["id"]

    # Forwarders: l2 (real fees) and grid nodes 0..5 on the path (mocked
    # fees: 1000 msat + 100 ppm on the amount they forward).
    mocked_forwarders = len(route) - 2
    mocked_fee_floor = mocked_forwarders * (1000 + amount * 100 // 1_000_000)
    assert info["fee_msat"] > mocked_fee_floor


def test_every_simulated_node_is_reachable(simulated):
    l1, l2, nodes, channels = simulated
    for node in nodes:
        info = l1.rpc.call("barqrouteinfo", {"dest_pubkey": node, "amount_msat": 10_000})
        assert info["route"][-1]["id"] == node


def test_direct_strategy_cannot_reach_simulated_nodes(simulated):
    l1, l2, nodes, channels = simulated
    with pytest.raises(RpcError, match="cannot route this payment"):
        l1.rpc.call("barqrouteinfo", {
            "dest_pubkey": nodes[5],
            "amount_msat": 10_000,
            "strategy": "direct",
        })
    # The real peer is still a direct route.
    info = l1.rpc.call("barqrouteinfo", {
        "dest_pubkey": l2.info["id"],
        "amount_msat": 10_000,
        "strategy": "direct",
    })
    assert info["route"][0]["channel"] == only_one(
        l1.rpc.listpeerchannels(l2.info["id"])["channels"]
    )["short_channel_id"]


def test_invalid_simulated_graph_disables_plugin(node_factory, tmp_path):
    spec = tmp_path / "graph.json"
    spec.write_text(json.dumps({"channels": [{"node1": "nope", "node2": "nope", "capacity_sat": 1}]}))
    node = node_factory.get_node(options={
        "plugin": barq_binary,
        "barq-dev-simulated-graph": str(spec),
    })
    # The init response disables the plugin; logged before "Server started".
    assert node.daemon.is_in_log("cannot load simulated graph")
    with pytest.raises(RpcError):
        node.rpc.call("barqsyncgossip", {})
