"""Serve Core Lightning's `pay` command with barq (issue #36).

With `barq-override-pay=true`, barq takes over `pay` through an
`rpc_command` hook filtered to `pay`, so anything that calls `pay` (users,
other plugins, test suites) uses barq. Hook filters need Core Lightning
v25.12 or later; on older versions barq does not register the hook.
"""
import hashlib
import os
import re
import subprocess

import pytest

from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import only_one
from pyln.client import Millisatoshi, RpcError

barq_binary = os.path.join(os.path.dirname(__file__), "../target/debug/barq-plugin")


def barq(**extra):
    return dict({"plugin": barq_binary}, **extra)


def cln_version():
    """Version of the lightningd the tests run, without starting a node."""
    out = subprocess.check_output(["lightningd", "--version"]).decode()
    match = re.match(r"v?(\d+)\.(\d+)", out.strip())
    return int(match.group(1)), int(match.group(2))


HAS_FILTERS = cln_version() >= (25, 12)
needs_filters = pytest.mark.skipif(
    not HAS_FILTERS, reason="rpc_command hook filters need Core Lightning v25.12 or later"
)


def network(node_factory, **barq_opts):
    """l1 (barq) -> l2 -> l3."""
    return node_factory.line_graph(
        3, opts=[barq(**barq_opts), barq(), barq()], wait_for_announce=True
    )


def overriding(node_factory):
    """l1 serves `pay` with barq; xpay must not also take it over."""
    opts = {"barq-override-pay": True}
    l1, l2, l3 = network(node_factory, **opts)
    if "xpay-handle-pay" in l1.rpc.listconfigs()["configs"]:
        l1.rpc.setconfig("xpay-handle-pay", False)
    return l1, l2, l3


def barq_records(node, payment_hash):
    try:
        return node.rpc.call("barqinspect", {"payment_hash": payment_hash})["payments"]
    except RpcError:
        return []


@needs_filters
def test_pay_is_served_by_barq(node_factory):
    l1, l2, l3 = overriding(node_factory)
    assert l1.daemon.is_in_log("barq serves the `pay` command")

    inv = l3.rpc.invoice(Millisatoshi("123sat"), "hook", "hook")["bolt11"]
    result = l1.rpc.pay(inv)

    # pay's own result format.
    assert result["status"] == "complete"
    assert result["parts"] == 1
    assert result["amount_msat"] == Millisatoshi(123_000)
    assert result["amount_sent_msat"] > result["amount_msat"]
    assert result["destination"] == l3.info["id"]
    preimage = bytes.fromhex(result["payment_preimage"])
    assert hashlib.sha256(preimage).hexdigest() == result["payment_hash"]
    assert only_one(l3.rpc.listinvoices("hook")["invoices"])["status"] == "paid"

    # barq, not Core Lightning's pay plugin, made the payment.
    assert l1.daemon.is_in_log("serving `pay` with barq")
    record = only_one(barq_records(l1, result["payment_hash"]))
    assert record["status"] == "complete"
    assert record["attempts"][0]["route"][0]["id"] == l2.info["id"]


@needs_filters
def test_pay_bolt12_and_amountless_invoices(node_factory):
    l1, l2, l3 = overriding(node_factory)

    offer = l3.rpc.offer("5000msat", "hook bolt12")["bolt12"]
    inv = l1.rpc.fetchinvoice(offer=offer)["invoice"]
    result = l1.rpc.pay(inv)
    assert result["status"] == "complete"
    assert only_one(barq_records(l1, result["payment_hash"]))["invoice_type"] == "bolt12"

    inv = l3.rpc.invoice("any", "any", "hook")["bolt11"]
    result = l1.rpc.pay(inv, amount_msat="10sat")
    assert result["amount_msat"] == Millisatoshi(10_000)
    assert only_one(l3.rpc.listinvoices("any")["invoices"])["status"] == "paid"


@needs_filters
def test_pay_errors_use_pay_codes(node_factory):
    l1, l2, l3 = overriding(node_factory)

    inv = l3.rpc.invoice(10_000, "gone", "hook")["bolt11"]
    l3.rpc.delinvoice("gone", "unpaid")
    with pytest.raises(RpcError) as err:
        l1.rpc.pay(inv)
    assert err.value.error["code"] == 203
    assert "WIRE_INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS" in err.value.error["message"]
    # barq's details stay available in the error data.
    assert len(err.value.error["data"]["attempts"]) == 1

    inv = l3.rpc.invoice(10**12, "huge", "hook")["bolt11"]
    with pytest.raises(RpcError) as err:
        l1.rpc.pay(inv)
    assert err.value.error["code"] == 205

    with pytest.raises(RpcError) as err:
        l1.rpc.call("pay", {"bolt11": inv, "amount_msat": "lots"})
    assert err.value.error["code"] == -32602


@needs_filters
def test_barq_defers_to_xpay(node_factory):
    """When xpay also takes over `pay`, barq steps aside: two plugins
    answering the same rpc_command crash Core Lightning."""
    l1, l2, l3 = network(node_factory, **{"barq-override-pay": True})
    configs = l1.rpc.listconfigs()["configs"]
    if not configs.get("xpay-handle-pay", {}).get("value_bool"):
        pytest.skip("xpay does not handle pay on this node")

    inv = l3.rpc.invoice(10_000, "xpay", "hook")["bolt11"]
    result = l1.rpc.pay(inv)
    assert result["status"] == "complete"
    assert l1.daemon.is_in_log("letting xpay pay")
    assert barq_records(l1, result["payment_hash"]) == []

    # Switching xpay off at runtime hands `pay` to barq.
    l1.rpc.setconfig("xpay-handle-pay", False)
    inv = l3.rpc.invoice(10_000, "barq", "hook")["bolt11"]
    result = l1.rpc.pay(inv)
    assert only_one(barq_records(l1, result["payment_hash"]))["status"] == "complete"


def test_pay_is_left_alone_by_default(node_factory):
    l1, l2, l3 = network(node_factory)
    inv = l3.rpc.invoice(10_000, "native", "hook")["bolt11"]
    result = l1.rpc.pay(inv)
    assert result["status"] == "complete"
    assert not l1.daemon.is_in_log("serving `pay` with barq")
    assert barq_records(l1, result["payment_hash"]) == []


@pytest.mark.skipif(HAS_FILTERS, reason="this Core Lightning supports rpc_command filters")
def test_old_core_lightning_keeps_its_pay(node_factory):
    """Without rpc_command filters barq must not register the hook: it would
    see every command, including its own, and deadlock."""
    l1, l2, l3 = network(node_factory, **{"barq-override-pay": True})
    assert l1.daemon.is_in_log("barq-override-pay needs Core Lightning v25.12 or later")

    inv = l3.rpc.invoice(10_000, "old", "hook")["bolt11"]
    result = l1.rpc.pay(inv)
    assert result["status"] == "complete"
    assert barq_records(l1, result["payment_hash"]) == []
    # barq itself still works.
    inv = l3.rpc.invoice(10_000, "barqpay", "hook")["bolt11"]
    assert l1.rpc.call("barqpay", {"invoice": inv})["status"] == "complete"
