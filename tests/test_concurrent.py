"""barq serves several payments at once: a payment that takes long does
not hold up the others, nor barq's other commands."""
import os
import re
import subprocess

import pytest

from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import only_one, wait_for, TIMEOUT
from pyln.client import RpcError

barq_binary = os.path.join(os.path.dirname(__file__), "../target/debug/barq-plugin")
hold_plugin = os.path.join(os.path.dirname(__file__), "plugins/hold_htlcs.py")

# `pay`'s error code for a payment already in flight.
PAY_IN_PROGRESS = 200


def cln_version():
    out = subprocess.check_output(["lightningd", "--version"]).decode()
    match = re.match(r"v?(\d+)\.(\d+)", out.strip())
    return int(match.group(1)), int(match.group(2))


def held(node, payment_hash):
    return node.rpc.call("heldhtlcs")["held"].get(payment_hash, 0)


def test_rpay_while_another_payment_is_pending(node_factory, executor):
    # l1 (barq) -> l2 -> l3, and l3 holds the HTLCs it is told to.
    l1, l2, l3 = node_factory.line_graph(
        3,
        opts=[{"plugin": barq_binary}, {}, {"plugin": hold_plugin}],
        wait_for_announce=True,
    )
    slow = l3.rpc.invoice(10_000, "slow", "slow")
    fast = l3.rpc.invoice(20_000, "fast", "fast")
    l3.rpc.call("holdhtlcs", {"payment_hash": slow["payment_hash"]})

    slow_pay = executor.submit(l1.rpc.call, "rpay", {"bolt11": slow["bolt11"]})
    try:
        wait_for(lambda: held(l3, slow["payment_hash"]) == 1)

        # The slow payment is in flight: another one goes through anyway.
        fast_pay = executor.submit(l1.rpc.call, "rpay", {"bolt11": fast["bolt11"]})
        result = fast_pay.result(TIMEOUT)
        assert result["status"] == "complete"
        assert result["payment_hash"] == fast["payment_hash"]
        assert only_one(l3.rpc.listinvoices("fast")["invoices"])["status"] == "paid"

        assert not slow_pay.done()
        assert only_one(l1.rpc.listsendpays(payment_hash=slow["payment_hash"])["payments"])["status"] == "pending"
        # barq's other commands answer too.
        recorded = l1.rpc.call("barqinspect", {"payment_hash": fast["payment_hash"]})["payments"]
        assert only_one(recorded)["status"] == "complete"

        # Paying the pending invoice again is refused like `pay` does,
        # whether through rpay or barqpay, and leaves the first payment be.
        for method, params in [("rpay", {"bolt11": slow["bolt11"]}),
                               ("barqpay", {"invoice": slow["bolt11"]})]:
            with pytest.raises(RpcError, match="in progress") as err:
                l1.rpc.call(method, params)
            assert err.value.error["code"] == PAY_IN_PROGRESS
        assert held(l3, slow["payment_hash"]) == 1
        assert not slow_pay.done()
    finally:
        l3.rpc.call("releasehtlcs", {"payment_hash": slow["payment_hash"]})

    result = slow_pay.result(TIMEOUT)
    assert result["status"] == "complete"
    assert result["payment_hash"] == slow["payment_hash"]
    assert only_one(l3.rpc.listinvoices("slow")["invoices"])["status"] == "paid"


@pytest.mark.skipif(cln_version() < (25, 12),
                    reason="rpc_command hook filters need Core Lightning v25.12 or later")
def test_pay_hook_while_another_payment_is_pending(node_factory, executor):
    """Same with barq serving `pay` through the rpc_command hook."""
    l1, l2, l3 = node_factory.line_graph(
        3,
        opts=[{"plugin": barq_binary, "barq-override-pay": True}, {}, {"plugin": hold_plugin}],
        wait_for_announce=True,
    )
    if "xpay-handle-pay" in l1.rpc.listconfigs()["configs"]:
        l1.rpc.setconfig("xpay-handle-pay", False)
    slow = l3.rpc.invoice(10_000, "slow", "slow")
    fast = l3.rpc.invoice(20_000, "fast", "fast")
    l3.rpc.call("holdhtlcs", {"payment_hash": slow["payment_hash"]})

    slow_pay = executor.submit(l1.rpc.pay, slow["bolt11"])
    try:
        wait_for(lambda: held(l3, slow["payment_hash"]) == 1)
        result = executor.submit(l1.rpc.pay, fast["bolt11"]).result(TIMEOUT)
        assert result["status"] == "complete"
        # barq paid it, not Core Lightning's pay.
        recorded = l1.rpc.call("barqinspect", {"payment_hash": fast["payment_hash"]})["payments"]
        assert only_one(recorded)["status"] == "complete"
        assert not slow_pay.done()
    finally:
        l3.rpc.call("releasehtlcs", {"payment_hash": slow["payment_hash"]})

    assert slow_pay.result(TIMEOUT)["status"] == "complete"
    assert only_one(l3.rpc.listinvoices("slow")["invoices"])["status"] == "paid"
