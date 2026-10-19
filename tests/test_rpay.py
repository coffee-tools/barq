"""`rpay`: barq behind `pay`'s interface (same parameters, result and error
codes), always served by barq. Unlike the `pay` takeover it needs no hook,
so it works on every Core Lightning version."""
import hashlib
import os

import pytest

from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import only_one
from pyln.client import Millisatoshi, RpcError

barq_binary = os.path.join(os.path.dirname(__file__), "../target/debug/barq-plugin")

# The fields Core Lightning's `pay` always returns on success.
PAY_FIELDS = {"payment_preimage", "payment_hash", "created_at", "parts",
              "amount_msat", "amount_sent_msat", "status"}


def network(node_factory):
    opts = {"plugin": barq_binary}
    return node_factory.line_graph(3, opts=[dict(opts) for _ in range(3)], wait_for_announce=True)


def recorded_by_barq(node, payment_hash):
    return node.rpc.call("barqinspect", {"payment_hash": payment_hash})["payments"]


def test_rpay_answers_like_pay(node_factory):
    l1, l2, l3 = network(node_factory)

    # Core Lightning's own pay, for reference.
    native = l1.rpc.pay(l3.rpc.invoice(10_000, "native", "native")["bolt11"])
    assert PAY_FIELDS <= set(native)

    inv = l3.rpc.invoice(Millisatoshi("50sat"), "rpay", "rpay")["bolt11"]
    result = l1.rpc.call("rpay", {"bolt11": inv})

    assert PAY_FIELDS <= set(result), set(result)
    assert set(result) <= set(native) | {"destination"}
    assert result["status"] == "complete"
    assert result["parts"] == 1
    assert result["amount_msat"] == Millisatoshi(50_000)
    assert result["amount_sent_msat"] > result["amount_msat"]
    assert result["destination"] == l3.info["id"]
    assert isinstance(result["created_at"], float)
    preimage = bytes.fromhex(result["payment_preimage"])
    assert hashlib.sha256(preimage).hexdigest() == result["payment_hash"]
    assert only_one(l3.rpc.listinvoices("rpay")["invoices"])["status"] == "paid"
    # barq made it: the payment is in its history with its route.
    record = only_one(recorded_by_barq(l1, result["payment_hash"]))
    assert record["attempts"][0]["route"][0]["id"] == l2.info["id"]


def test_rpay_takes_pay_parameters(node_factory):
    l1, l2, l3 = network(node_factory)

    # By position, as `lightning-cli rpay <bolt11> <amount_msat> <label>`.
    inv = l3.rpc.invoice("any", "positional", "rpay")["bolt11"]
    result = l1.rpc.call("rpay", [inv, "12sat", "my label"])
    assert result["amount_msat"] == Millisatoshi(12_000)
    record = only_one(recorded_by_barq(l1, result["payment_hash"]))
    assert record["label"] == "my label"

    # pay's tuning parameters are accepted and ignored.
    inv = l3.rpc.invoice(5_000, "extra", "rpay")["bolt11"]
    result = l1.rpc.call("rpay", {"bolt11": inv, "maxfeepercent": 0.5, "retry_for": 60, "riskfactor": 10})
    assert result["status"] == "complete"

    # BOLT12 invoices too.
    offer = l3.rpc.offer("7000msat", "rpay bolt12")["bolt12"]
    b12 = l1.rpc.fetchinvoice(offer=offer)["invoice"]
    result = l1.rpc.call("rpay", {"bolt11": b12})
    assert result["status"] == "complete"
    assert only_one(recorded_by_barq(l1, result["payment_hash"]))["invoice_type"] == "bolt12"


def test_rpay_uses_pay_error_codes(node_factory):
    l1, l2, l3 = network(node_factory)

    inv = l3.rpc.invoice(10_000, "gone", "rpay")["bolt11"]
    l3.rpc.delinvoice("gone", "unpaid")
    with pytest.raises(RpcError) as err:
        l1.rpc.call("rpay", {"bolt11": inv})
    assert err.value.error["code"] == 203
    assert len(err.value.error["data"]["attempts"]) == 1

    inv = l3.rpc.invoice(10**12, "huge", "rpay")["bolt11"]
    with pytest.raises(RpcError) as err:
        l1.rpc.call("rpay", {"bolt11": inv})
    assert err.value.error["code"] == 205

    for bad in ({}, {"bolt11": inv, "amount_msat": "lots"}):
        with pytest.raises(RpcError) as err:
            l1.rpc.call("rpay", bad)
        assert err.value.error["code"] == -32602
