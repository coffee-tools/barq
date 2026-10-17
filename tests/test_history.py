"""Payment history and `barqinspect` (issues #7 and #8)."""
import hashlib
import os

import pytest

from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import only_one, wait_for
from pyln.client import RpcError

from test_strategy_selection import diamond, scid_between

barq_binary = os.path.join(os.path.dirname(__file__), "../target/debug/barq-plugin")
opts = {"plugin": barq_binary}


def messages(record):
    return [e["message"] for e in record["events"]]


def test_completed_payment_is_recorded(node_factory):
    l1, l2, l3 = node_factory.line_graph(3, opts=[dict(opts) for _ in range(3)], wait_for_announce=True)
    inv = l3.rpc.invoice(50_000, "hist", "history")["bolt11"]
    result = l1.rpc.call("barqpay", {"invoice": inv, "label": "coffee"})

    record = l1.rpc.call("barqinspect", {"id": result["id"]})
    assert record["id"] == result["id"]
    assert record["status"] == "complete"
    assert record["invoice"] == inv
    assert record["invoice_type"] == "bolt11"
    assert record["payment_hash"] == result["payment_hash"]
    assert record["destination"] == l3.info["id"]
    assert record["label"] == "coffee"
    assert record["amount_msat"] == 50_000
    assert record["amount_sent_msat"] == result["amount_sent_msat"]
    assert record["fee_msat"] == result["fee_msat"]
    assert record["payment_preimage"] == result["payment_preimage"]
    preimage = bytes.fromhex(record["payment_preimage"])
    assert hashlib.sha256(preimage).hexdigest() == record["payment_hash"]
    assert record["completed_at"] >= record["created_at"]

    attempt = only_one(record["attempts"])
    assert attempt["status"] == "complete"
    assert [h["id"] for h in attempt["route"]] == [l2.info["id"], l3.info["id"]]
    assert attempt["route"][0]["channel"] == scid_between(l1, l2)

    journal = messages(record)
    assert journal[0].startswith("paying 50000 msat")
    assert any(m.startswith("attempt 1/") for m in journal)
    assert journal[-1].startswith("complete")
    # The positional form works too: `lightning-cli barqinspect 1`.
    assert l1.rpc.call("barqinspect", [result["id"]])["id"] == result["id"]


def test_failed_payment_is_recorded(node_factory):
    l1, l2, l3 = node_factory.line_graph(3, opts=[dict(opts) for _ in range(3)], wait_for_announce=True)
    inv = l3.rpc.invoice(50_000, "gone", "history")["bolt11"]
    l3.rpc.delinvoice("gone", "unpaid")

    with pytest.raises(RpcError) as err:
        l1.rpc.call("barqpay", {"invoice": inv})
    payment_id = err.value.error["data"]["id"]

    record = l1.rpc.call("barqinspect", {"id": payment_id})
    assert record["status"] == "failed"
    assert record["error"]["code"] == 203
    assert "WIRE_INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS" in record["error"]["message"]
    attempt = only_one(record["attempts"])
    assert attempt["status"] == "failed"
    assert "payment_preimage" not in record
    assert any("failed at the destination" in m for m in messages(record))


def test_retried_payment_shows_every_attempt(node_factory):
    l1, l2, l3, l4 = diamond(node_factory)
    inv = l4.rpc.invoice(100_000, "retry", "history")["bolt11"]
    result = l1.rpc.call("barqpay", {"invoice": inv})

    record = l1.rpc.call("barqinspect", {"id": result["id"]})
    assert record["status"] == "complete"
    assert [a["status"] for a in record["attempts"]] == ["failed", "complete"]
    assert record["attempts"][0]["erring_channel"] == scid_between(l2, l4)
    journal = messages(record)
    assert any("attempt 1 failed" in m and "excluding channel" in m for m in journal)
    assert any(m.startswith("attempt 2/") for m in journal)


def test_lookup_by_payment_hash_and_listing(node_factory):
    l1, l2 = node_factory.line_graph(2, opts=[dict(opts), dict(opts)], wait_for_announce=True)
    ids = []
    for label in ["a", "b", "c"]:
        inv = l2.rpc.invoice(1_000, label, "history")["bolt11"]
        ids.append(l1.rpc.call("barqpay", {"invoice": inv})["id"])
    assert ids == sorted(ids)

    listed = l1.rpc.call("barqinspect", {})["payments"]
    assert [p["id"] for p in listed] == list(reversed(ids))
    assert all(p["status"] == "complete" and p["attempts"] == 1 for p in listed)
    assert len(l1.rpc.call("barqinspect", {"limit": 2})["payments"]) == 2

    payment_hash = l1.rpc.call("barqinspect", {"id": ids[1]})["payment_hash"]
    by_hash = l1.rpc.call("barqinspect", {"payment_hash": payment_hash})["payments"]
    assert [p["id"] for p in by_hash] == [ids[1]]

    with pytest.raises(RpcError, match="No payment with id 999"):
        l1.rpc.call("barqinspect", {"id": 999})
    with pytest.raises(RpcError, match="No payment with payment_hash"):
        l1.rpc.call("barqinspect", {"payment_hash": "00" * 32})


def test_history_survives_a_restart(node_factory):
    restartable = dict(opts, may_reconnect=True)
    l1, l2 = node_factory.line_graph(2, opts=[dict(restartable), dict(restartable)], wait_for_announce=True)
    inv = l2.rpc.invoice(1_000, "keep", "history")["bolt11"]
    first = l1.rpc.call("barqpay", {"invoice": inv})["id"]
    path = os.path.join(l1.daemon.lightning_dir, "regtest", "barq", "payments", f"{first}.json")
    assert os.path.exists(path)

    l1.restart()
    assert l1.rpc.call("barqinspect", {"id": first})["status"] == "complete"
    l1.rpc.connect(l2.info["id"], "localhost", l2.port)
    wait_for(lambda: only_one(l1.rpc.listpeerchannels(l2.info["id"])["channels"])["state"] == "CHANNELD_NORMAL"
             and only_one(l1.rpc.listpeerchannels(l2.info["id"])["channels"])["peer_connected"])
    # New payments continue the numbering.
    inv = l2.rpc.invoice(1_000, "next", "history")["bolt11"]
    assert l1.rpc.call("barqpay", {"invoice": inv})["id"] == first + 1


def test_payment_without_route_is_recorded(node_factory):
    l1, l2, l3 = node_factory.line_graph(3, opts=[dict(opts) for _ in range(3)], wait_for_announce=True)
    inv = l3.rpc.invoice(10**12, "huge", "history")["bolt11"]
    with pytest.raises(RpcError, match="No route found") as err:
        l1.rpc.call("barqpay", {"invoice": inv})
    record = l1.rpc.call("barqinspect", {"id": err.value.error["data"]["id"]})
    assert record["status"] == "failed"
    assert record["attempts"] == []
    assert any("has no route" in m for m in messages(record))
