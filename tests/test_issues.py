"""Regression tests, one per GitHub issue closed by the LDK rewrite.

Each test names the issue it proves fixed, see
https://github.com/coffee-tools/barq/issues
"""
import os

from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import only_one, wait_for

barq_binary = os.path.join(os.path.dirname(__file__), "../target/debug/barq-plugin")
opts = {"plugin": barq_binary}


def three_nodes(node_factory):
    return node_factory.line_graph(
        3, opts=[dict(opts) for _ in range(3)], wait_for_announce=True
    )


def scid_between(a, b):
    return only_one(a.rpc.listpeerchannels(b.info["id"])["channels"])["short_channel_id"]


def test_issue_5_plugin_log_env_does_not_crash(node_factory):
    """#5: cln4rust's `log` feature parsed CLN_PLUGIN_LOG as a log level and
    panicked on values like `cln_plugin=trace,...`, which also crashed
    lightningd. barq installs its own logger, so any value is fine."""
    for value in ["cln_plugin=trace,cln_rpc=trace,cln_grpc=trace,debug", "not-a-level"]:
        node = node_factory.get_node(options=dict(opts), start=False)
        node.daemon.env["CLN_PLUGIN_LOG"] = value
        node.start()
        assert node.daemon.is_in_log("barq initialized on regtest")
        assert node.rpc.call("barqsyncgossip", {})["gossip_store"]["enabled"] is True
        assert not node.daemon.is_in_log("panicked")


def test_issue_18_and_39_route_info(node_factory):
    """#18: `cltv` is optional in `barqrouteinfo`.
    #39: channel ids handed to and returned from LDK are Core Lightning's
    `BLOCKxTXxOUT` short channel ids, not raw integers or hex."""
    l1, l2, l3 = three_nodes(node_factory)
    request = {"dest_pubkey": l3.info["id"], "amount_msat": 123_000}

    without_cltv = l1.rpc.call("barqrouteinfo", request)
    with_cltv = l1.rpc.call("barqrouteinfo", dict(request, cltv=144))

    for info in (without_cltv, with_cltv):
        assert [h["id"] for h in info["route"]] == [l2.info["id"], l3.info["id"]]
        assert [h["channel"] for h in info["route"]] == [
            scid_between(l1, l2),
            scid_between(l2, l3),
        ]
    # The final CLTV defaults to 18; LDK may add a random shadow offset.
    assert without_cltv["route"][-1]["cltv_expiry_delta"] >= 18
    assert with_cltv["route"][-1]["cltv_expiry_delta"] >= 144


def test_issue_38_channel_updates_reach_ldk(node_factory):
    """#38: converting Core Lightning gossip to LDK must keep the
    channel_update data, or LDK routes with unknown fees. The fee barq
    computes must match l2's advertised policy on the l2 -> l3 channel."""
    l1, l2, l3 = three_nodes(node_factory)
    amount = 123_000
    l2_l3 = scid_between(l2, l3)
    policy = only_one(
        [c for c in l1.rpc.listchannels(l2_l3)["channels"] if c["source"] == l2.info["id"]]
    )
    expected_fee = policy["base_fee_millisatoshi"] + amount * policy["fee_per_millionth"] // 1_000_000

    info = l1.rpc.call("barqrouteinfo", {"dest_pubkey": l3.info["id"], "amount_msat": amount})

    assert info["fee_msat"] == expected_fee, (info, policy)
    # Hop 1 is l2: it is paid the forwarding fee on top of the amount.
    assert info["route"][0]["fee_msat"] == expected_fee
    assert info["route"][-1]["fee_msat"] == amount


def test_issue_21_graph_is_loaded_once_and_persisted(node_factory):
    """#21: the network graph is kept in memory and read incrementally from
    the gossip_store, not rebuilt on every command, and it survives a
    restart."""
    l1, l2, l3 = three_nodes(node_factory)

    first = l1.rpc.call("barqsyncgossip", {})
    assert first["channels"] >= 2 and first["nodes"] >= 3

    # Nothing new: the reader resumes where it stopped.
    second = l1.rpc.call("barqsyncgossip", {})
    assert second["gossip_store"]["channel_announcements"] == 0
    assert second["gossip_store"]["offset"] == first["gossip_store"]["offset"]

    graph_file = os.path.join(l1.daemon.lightning_dir, "regtest", "barq", "network_graph.regtest.bin")
    assert os.path.exists(graph_file)

    assert not l1.daemon.is_in_log("loaded network graph from")
    l1.restart()
    # Logged during init, before the "Server started" line pyln waits for.
    assert l1.daemon.is_in_log("loaded network graph from")
    wait_for(lambda: l1.rpc.call("barqsyncgossip", {})["channels"] >= 2)
