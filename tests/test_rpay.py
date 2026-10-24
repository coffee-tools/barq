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


def test_rpay_fee_limits_and_exclusions(node_factory):
    """pay's fee limit (maxfeepercent, exemptfee, maxfee) and `exclude`."""
    l1, l2, l3 = node_factory.line_graph(
        3,
        opts=[{"plugin": barq_binary}, {"fee-base": 5001, "fee-per-satoshi": 0}, {}],
        wait_for_announce=True,
    )
    # 5001msat of fees for 1msat: over 0.5% and over the 5000msat exemption.
    inv = l3.rpc.invoice(1, "tiny", "tiny")["bolt11"]
    with pytest.raises(RpcError, match="Could not find route without excessive cost") as err:
        l1.rpc.call("rpay", {"bolt11": inv})
    assert err.value.error["code"] == 206
    assert l1.rpc.call("rpay", {"bolt11": inv, "exemptfee": 5001})["status"] == "complete"

    inv = l3.rpc.invoice(10_000, "maxfee", "maxfee")["bolt11"]
    with pytest.raises(RpcError, match="excessive cost"):
        l1.rpc.call("rpay", {"bolt11": inv, "maxfee": 5000})
    with pytest.raises(RpcError, match="cannot specify maxfeepercent"):
        l1.rpc.call("rpay", {"bolt11": inv, "maxfee": 6000, "maxfeepercent": 1})

    with pytest.raises(RpcError, match="Payer is manually excluded"):
        l1.rpc.call("rpay", {"bolt11": inv, "exclude": [l1.info["id"]]})
    with pytest.raises(RpcError, match="Payee is manually excluded"):
        l1.rpc.call("rpay", {"bolt11": inv, "exclude": [l3.info["id"]]})
    scid23 = l2.get_channel_scid(l3)
    for excluded in (l2.info["id"], scid23, f"{scid23}/0"):
        with pytest.raises(RpcError, match="is not reachable directly") as err:
            l1.rpc.call("rpay", {"bolt11": inv, "exclude": [excluded], "maxfee": 10_000})
        assert err.value.error["code"] == 205
    assert l1.rpc.call("rpay", {"bolt11": inv, "maxfee": 5001})["status"] == "complete"


def test_rpay_like_pay_on_repeats_and_self(node_factory):
    """An invoice already paid answers with that payment; our own invoices
    and offers are paid locally; partial_msat and description are checked."""
    l1, l2, l3 = network(node_factory)

    inv = l3.rpc.invoice(10_000, "twice", "twice")["bolt11"]
    first = l1.rpc.call("rpay", {"bolt11": inv})
    again = l1.rpc.call("rpay", {"bolt11": inv})
    assert again["payment_preimage"] == first["payment_preimage"]
    assert len(l1.rpc.listsendpays(inv)["payments"]) == 1
    # Every attempt is a part of the payment's group, like pay.
    part = only_one(l1.rpc.listsendpays(inv)["payments"])
    assert (part["groupid"], part["partid"]) == (1, 1)

    # BOLT11 to ourselves.
    own = l1.rpc.invoice(5_000, "self", "self")["bolt11"]
    assert l1.rpc.call("rpay", {"bolt11": own})["status"] == "complete"
    assert only_one(l1.rpc.listinvoices("self")["invoices"])["status"] == "paid"
    # BOLT12 to ourselves, through our own blinded path.
    offer = l1.rpc.offer("3000msat", "self offer")["bolt12"]
    b12 = l1.rpc.fetchinvoice(offer=offer)["invoice"]
    assert l1.rpc.call("rpay", {"bolt11": b12})["status"] == "complete"

    inv = l3.rpc.invoice(10_000, "partial", "partial")["bolt11"]
    with pytest.raises(RpcError, match="partial_msat must be less or equal"):
        l1.rpc.call("rpay", {"bolt11": inv, "partial_msat": 10_001})

    description = "a slice of watermelon"
    inv = l3.rpc.invoice(1_000, "deschash", description, deschashonly=True)["bolt11"]
    with pytest.raises(RpcError, match="does not match description"):
        l1.rpc.call("rpay", {"bolt11": inv, "description": description[:-1]})
    l1.rpc.call("rpay", {"bolt11": inv, "description": description})
    assert only_one(l1.rpc.listpays(inv)["pays"])["description"] == description
