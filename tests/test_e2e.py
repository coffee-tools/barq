"""End-to-end payment tests: l1 pays l3 through l2 with barq.

Every test runs once for a BOLT11 invoice and once for a BOLT12 invoice
(blinded path). barq sends a single HTLC per payment, so these tests also
check that nothing in Core Lightning or in the invoices forces a
multi-part payment.
"""
import hashlib
import os

import pytest

from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import only_one
from pyln.client import RpcError

barq_binary = os.path.join(os.path.dirname(__file__), "../target/debug/barq-plugin")

# basic_mpp, BOLT9 bits 16/17.
BASIC_MPP_BITS = (16, 17)

KINDS = ["bolt11", "bolt12"]


def three_nodes(node_factory):
    """l1 -> l2 -> l3, all running barq, channels announced."""
    opts = [{"plugin": barq_binary} for _ in range(3)]
    return node_factory.line_graph(3, opts=opts, wait_for_announce=True)


def has_feature(features_hex, bits):
    if not features_hex:
        return False
    value = int(features_hex, 16)
    return any((value >> bit) & 1 for bit in bits)


def new_invoice(kind, payer, payee, amount_msat, label):
    """Return an invoice string issued by `payee` for `payer`."""
    if kind == "bolt11":
        return payee.rpc.invoice(amount_msat, label, "barq e2e")["bolt11"]
    offer = payee.rpc.call("offer", {"amount": f"{amount_msat}msat", "description": label})
    return payer.rpc.call("fetchinvoice", {"offer": offer["bolt12"]})["invoice"]


def payee_invoice(payee, invstring):
    return only_one(payee.rpc.call("listinvoices", {"invstring": invstring})["invoices"])


def payment_hash_of(node, invstring):
    decoded = node.rpc.decode(invstring)
    return decoded.get("payment_hash") or decoded["invoice_payment_hash"]


def assert_not_paid(payer, payee, invstring):
    payment_hash = payment_hash_of(payer, invstring)
    parts = payer.rpc.listsendpays(payment_hash=payment_hash)["payments"]
    assert all(p["status"] != "complete" for p in parts), parts
    if payee is not None:
        assert payee_invoice(payee, invstring)["status"] == "unpaid"


@pytest.mark.parametrize("kind", KINDS)
def test_e2e_pay(node_factory, kind):
    """Pay over two hops with a single HTLC, even though the invoice
    advertises multi-part support."""
    l1, l2, l3 = three_nodes(node_factory)
    amount = 50_000
    inv = new_invoice(kind, l1, l3, amount, f"e2e-pay-{kind}")

    decoded = l1.rpc.decode(inv)
    assert decoded["type"] == f"{kind} invoice"
    features = decoded.get("features") if kind == "bolt11" else decoded.get("invoice_features")
    # Core Lightning invoices offer MPP; barq must not need it.
    assert has_feature(features, BASIC_MPP_BITS), features

    result = l1.rpc.call("barqpay", {"invoice": inv})

    assert result["status"] == "complete"
    assert result["invoice_type"] == kind
    assert result["strategy"] == "probabilistic"
    assert result["amount_msat"] == amount
    assert result["fee_msat"] > 0, "two hops must pay a routing fee to l2"
    assert result["amount_sent_msat"] == amount + result["fee_msat"]
    assert result["route"][0]["id"] == l2.info["id"]
    if kind == "bolt11":
        assert [h["id"] for h in result["route"]] == [l2.info["id"], l3.info["id"]]
        assert result["blinded_hops"] == 0
    else:
        assert result["blinded_hops"] >= 1

    preimage = bytes.fromhex(result["payment_preimage"])
    assert hashlib.sha256(preimage).hexdigest() == result["payment_hash"]

    # Exactly one part on the sender side.
    parts = l1.rpc.listsendpays(payment_hash=result["payment_hash"])["payments"]
    assert len(parts) == 1, parts
    assert parts[0]["status"] == "complete"
    # Like `pay`: the first attempt is part 1 of group 1.
    assert (parts[0]["groupid"], parts[0]["partid"]) == (1, 1)

    # The recipient got the whole amount in that single HTLC.
    received = payee_invoice(l3, inv)
    assert received["status"] == "paid"
    assert received["amount_received_msat"] == amount
    assert received["payment_preimage"] == result["payment_preimage"]


@pytest.mark.parametrize("kind", KINDS)
def test_e2e_fail_recipient_rejects(node_factory, kind):
    """The HTLC reaches l3, which no longer knows the invoice and fails it."""
    l1, l2, l3 = three_nodes(node_factory)
    inv = new_invoice(kind, l1, l3, 50_000, f"e2e-reject-{kind}")
    label = payee_invoice(l3, inv)["label"]
    l3.rpc.delinvoice(label, "unpaid")

    with pytest.raises(RpcError) as err:
        l1.rpc.call("barqpay", {"invoice": inv})

    error = err.value.error
    data = error["data"]
    if kind == "bolt11":
        # barq maps the failure back onto its route: the final node
        # rejected the payment, so retrying another route is pointless.
        assert error["code"] == 203, error
        assert data["failcodename"] == "WIRE_INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS"
        assert data["erring_node"] == l3.info["id"]
        assert data["erring_in_blinded_path"] is False
    else:
        # Inside a blinded path the failure is masked by design.
        assert error["code"] in (203, 204), error
        assert data["failcodename"] in (
            "WIRE_INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS",
            "WIRE_INVALID_ONION_BLINDING",
        ), data
        assert data["erring_in_blinded_path"] is True

    # The destination rejected it: barq does not try another route.
    assert len(data["attempts"]) == 1
    assert_not_paid(l1, None, inv)
    assert not l3.rpc.call("listinvoices", {"invstring": inv})["invoices"]


@pytest.mark.parametrize("kind", KINDS)
def test_e2e_fail_in_route(node_factory, kind):
    """l3 goes offline after issuing the invoice: l2 cannot forward."""
    l1, l2, l3 = three_nodes(node_factory)
    inv = new_invoice(kind, l1, l3, 50_000, f"e2e-route-{kind}")
    l2_l3 = only_one(l2.rpc.listpeerchannels(l3.info["id"])["channels"])["short_channel_id"]
    l3.stop()

    with pytest.raises(RpcError) as err:
        l1.rpc.call("barqpay", {"invoice": inv})

    error = err.value.error
    assert error["code"] == 204, error
    data = error["data"]
    assert data["erring_index"] == 1
    assert data["erring_node"] == l2.info["id"], data
    assert data["erring_channel"] == l2_l3, data
    assert data["erring_in_blinded_path"] is False
    assert data["failcodename"] in (
        "WIRE_TEMPORARY_CHANNEL_FAILURE",
        "WIRE_UNKNOWN_NEXT_PEER",
    ), data
    assert l1.daemon.is_in_log("failed at FailurePoint")
    assert_not_paid(l1, None, inv)

    # The plugin is still healthy after a failure.
    l1.rpc.call("barqsyncgossip", {})


@pytest.mark.parametrize("kind", KINDS)
def test_e2e_fail_no_route(node_factory, kind):
    """More than any single path can carry: barq fails before sending
    anything instead of splitting the payment."""
    l1, l2, l3 = three_nodes(node_factory)
    capacity_msat = only_one(l1.rpc.listpeerchannels(l2.info["id"])["channels"])["total_msat"]
    amount = capacity_msat + 1_000_000
    inv = new_invoice(kind, l1, l3, amount, f"e2e-noroute-{kind}")

    with pytest.raises(RpcError, match="Failed to find route"):
        l1.rpc.call("barqpay", {"invoice": inv})

    payment_hash = payment_hash_of(l1, inv)
    assert l1.rpc.listsendpays(payment_hash=payment_hash)["payments"] == []
    assert payee_invoice(l3, inv)["status"] == "unpaid"


def test_e2e_stale_fee_learned_from_failure(node_factory):
    """l2 raises its fee towards l3 but its gossip never reaches l1: the
    first barqpay fails with fee_insufficient, barq learns the new policy
    from the channel_update carried by the error, and the second barqpay
    succeeds without any gossip sync in between."""
    l1, l2, l3 = three_nodes(node_factory)
    l2_l3 = only_one(l2.rpc.listpeerchannels(l3.info["id"])["channels"])["short_channel_id"]

    def l1_view():
        """l2 -> l3 as l1's gossipd knows it."""
        channels = l1.rpc.listchannels(l2_l3)["channels"]
        return only_one([c for c in channels if c["source"] == l2.info["id"]])

    old = l1_view()
    # barq's graph holds the old policy before the gossip stops flowing.
    assert l1.rpc.call("barqsyncgossip", {})["channels"] == 2

    # l2 stops streaming gossip to its peers, then raises its fee at once
    # (setchannel keeps honouring the old fee for 10 minutes by default).
    l2.rpc.call("dev-suppress-gossip", {})
    new_base = old["base_fee_millisatoshi"] + 5_000
    new_ppm = old["fee_per_millionth"] + 2_000
    l2.rpc.setchannel(l2_l3, feebase=new_base, feeppm=new_ppm, enforcedelay=0)
    assert l1_view()["base_fee_millisatoshi"] == old["base_fee_millisatoshi"]

    amount = 100_000
    inv = new_invoice("bolt11", l1, l3, amount, "e2e-stale-fee")
    result = l1.rpc.call("barqpay", {"invoice": inv})

    # The first attempt failed with fee_insufficient on l2 -> l3 ...
    first, second = result["attempts"]
    assert first["status"] == "failed", first
    assert "WIRE_FEE_INSUFFICIENT" in first["error"], first
    assert first["erring_channel"] == l2_l3, first
    assert l1.daemon.is_in_log(f"applied channel_update for {l2_l3}")
    # A fee error is not a liquidity failure: the scorer is left alone,
    # and the channel is tried again with the new policy.
    assert l1.daemon.is_in_log("failed at FailurePoint .*: Policy")
    assert second["status"] == "complete", second
    assert [h["channel"] for h in second["route"]] == [h["channel"] for h in first["route"]]

    # l1's gossipd learnt the update through addgossip, not through gossip.
    view = l1_view()
    assert view["base_fee_millisatoshi"] == new_base, view
    assert view["fee_per_millionth"] == new_ppm, view

    # Both attempts are parts of one payment, like `pay` makes them.
    parts = l1.rpc.listsendpays(payment_hash=result["payment_hash"])["payments"]
    assert sorted((p["groupid"], p["partid"], p["status"]) for p in parts) == [
        (1, 1, "failed"),
        (1, 2, "complete"),
    ]
    assert result["status"] == "complete"
    assert [h["id"] for h in result["route"]] == [l2.info["id"], l3.info["id"]]
    assert result["fee_msat"] == new_base + amount * new_ppm // 1_000_000, result
    assert payee_invoice(l3, inv)["status"] == "paid"
