"""Strategy selection: forcing a strategy (#10), ranking strategies and
falling back to the next route when one fails (#16)."""
import os

import pytest

from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import only_one, wait_for
from pyln.client import Millisatoshi, RpcError

here = os.path.dirname(__file__)
barq_binary = os.path.join(here, "../target/debug/barq-plugin")
fail_forwards = os.path.join(here, "plugins/fail_forwards.py")


def barq(**extra):
    return dict({"plugin": barq_binary}, **extra)


def scid_between(a, b):
    return only_one(a.rpc.listpeerchannels(b.info["id"])["channels"])["short_channel_id"]


def wait_for_channels(node, count):
    """Wait until `node` knows `count` channels in both directions."""
    wait_for(lambda: len(node.rpc.listchannels()["channels"]) == 2 * count)


def test_auto_selection_prefers_direct_for_a_peer(node_factory):
    l1, l2 = node_factory.line_graph(2, opts=[barq(), barq()], wait_for_announce=True)

    info = l1.rpc.call("barqrouteinfo", {"dest_pubkey": l2.info["id"], "amount_msat": 10_000})
    # Both strategies propose the same zero-fee route: it is ranked once,
    # under the first strategy.
    assert info["strategy"] == "direct"
    assert [c["strategy"] for c in info["candidates"]] == ["direct"]
    assert info["candidates"][0]["fee_msat"] == 0

    inv = l2.rpc.invoice(10_000, "auto", "auto")["bolt11"]
    result = l1.rpc.call("barqpay", {"invoice": inv})
    assert result["strategy"] == "direct"
    assert [a["status"] for a in result["attempts"]] == ["complete"]


def test_forced_strategy(node_factory):
    """#10: the caller can force a strategy; nothing else is tried."""
    l1, l2, l3 = node_factory.line_graph(3, opts=[barq()] * 3, wait_for_announce=True)

    inv = l2.rpc.invoice(10_000, "forced", "forced")["bolt11"]
    result = l1.rpc.call("barqpay", {"invoice": inv, "strategy": "probabilistic"})
    assert result["strategy"] == "probabilistic"

    # Forcing a strategy that cannot route gives no fallback.
    inv = l3.rpc.invoice(10_000, "forced-direct", "forced")["bolt11"]
    with pytest.raises(RpcError, match="cannot route this payment"):
        l1.rpc.call("barqpay", {"invoice": inv, "strategy": "direct"})
    payment_hash = l1.rpc.decode(inv)["payment_hash"]
    assert l1.rpc.listsendpays(payment_hash=payment_hash)["payments"] == []

    with pytest.raises(RpcError, match="not found"):
        l1.rpc.call("barqpay", {"invoice": inv, "strategy": "teleport"})


def test_falls_back_when_the_direct_channel_is_too_small(node_factory):
    """#16: l1 has a small channel to l3 and a big path through l2. Paying
    more than the small channel holds ranks the probabilistic route."""
    l1, l2, l3 = node_factory.get_nodes(3, opts=[barq()] * 3)
    node_factory.join_nodes([l1, l2, l3], wait_for_announce=True)
    node_factory.join_nodes([l1, l3], fundamount=50_000, wait_for_announce=True)
    wait_for_channels(l1, 3)

    amount = Millisatoshi("200000sat")
    info = l1.rpc.call("barqrouteinfo", {"dest_pubkey": l3.info["id"], "amount_msat": int(amount)})
    assert info["strategy"] == "probabilistic"
    assert info["route"][0]["id"] == l2.info["id"]
    direct = only_one([r for r in info["rejected"] if r["strategy"] == "direct"])
    assert "can carry" in direct["reason"]

    inv = l3.rpc.invoice(amount, "fallback", "fallback")["bolt11"]
    result = l1.rpc.call("barqpay", {"invoice": inv})
    assert result["status"] == "complete"
    assert result["strategy"] == "probabilistic"
    assert [h["id"] for h in result["route"]] == [l2.info["id"], l3.info["id"]]


def diamond(node_factory):
    """l1 -> l2 -> l4 and l1 -> l3 -> l4. l2 fails every forward and is the
    cheaper path, so the first route always fails in the middle."""
    l1, l2, l3, l4 = node_factory.get_nodes(4, opts=[
        barq(),
        {"plugin": fail_forwards},
        {"fee-base": 20_000},
        {},
    ])
    node_factory.join_nodes([l1, l2, l4], wait_for_announce=True)
    node_factory.join_nodes([l1, l3, l4], wait_for_announce=True)
    wait_for_channels(l1, 4)
    return l1, l2, l3, l4


def test_retries_another_route_after_a_failure_in_the_middle(node_factory):
    """#16: the first route fails at l2; barq excludes the failing channel,
    re-ranks, and pays through l3."""
    l1, l2, l3, l4 = diamond(node_factory)
    l2_l4 = scid_between(l2, l4)

    inv = l4.rpc.invoice(100_000, "retry", "retry")["bolt11"]
    result = l1.rpc.call("barqpay", {"invoice": inv})

    assert result["status"] == "complete"
    attempts = result["attempts"]
    assert [a["status"] for a in attempts] == ["failed", "complete"]
    assert attempts[0]["route"][0]["id"] == l2.info["id"]
    assert attempts[0]["erring_channel"] == l2_l4
    assert "WIRE_TEMPORARY_NODE_FAILURE" in attempts[0]["error"]
    assert [h["id"] for h in result["route"]] == [l3.info["id"], l4.info["id"]]
    # The second route was more expensive: it was ranked lower.
    assert attempts[1]["fee_msat"] > attempts[0]["fee_msat"]
    l2.daemon.wait_for_log("failing forward to")

    payment_hash = l1.rpc.decode(inv)["payment_hash"]
    statuses = sorted(p["status"] for p in l1.rpc.listsendpays(payment_hash=payment_hash)["payments"])
    assert statuses == ["complete", "failed"]
    assert only_one(l4.rpc.listinvoices("retry")["invoices"])["status"] == "paid"


def test_gives_up_after_max_attempts(node_factory):
    """#16: with a single attempt allowed, the failure is returned with the
    attempt that was made."""
    l1, l2, l3, l4 = diamond(node_factory)

    inv = l4.rpc.invoice(100_000, "once", "once")["bolt11"]
    with pytest.raises(RpcError) as err:
        l1.rpc.call("barqpay", {"invoice": inv, "max_attempts": 1})

    error = err.value.error
    assert error["code"] == 204
    data = error["data"]
    assert data["erring_node"] == l2.info["id"]
    assert len(data["attempts"]) == 1
    assert data["attempts"][0]["status"] == "failed"
    assert only_one(l4.rpc.listinvoices("once")["invoices"])["status"] == "unpaid"


def test_max_attempts_option(node_factory):
    """The `barq-max-attempts` option sets the default."""
    node = node_factory.get_node(options=barq(**{"barq-max-attempts": 1}))
    configs = node.rpc.listconfigs("barq-max-attempts")["configs"]
    assert configs["barq-max-attempts"]["value_int"] == 1
