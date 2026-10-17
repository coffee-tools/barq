import os
import pytest

from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import only_one
from pyln.client import Millisatoshi
from pyln.client import RpcError

barq_binary = os.path.join(os.path.dirname(__file__), "../target/debug/barq-plugin")
opts = {"plugin": barq_binary}


def test_init(node_factory):
    """This is just a monkey test to check that the
    test env is configured fine"""
    node = node_factory.get_node(options=opts)
    assert node.daemon.is_in_log("barq initialized on regtest")
    # The graph is empty but the command works.
    info = node.rpc.call("barqsyncgossip", {})
    assert info["rapid_gossip_sync"]["enabled"] is False
    assert info["gossip_store"]["enabled"] is True


def test_pay_amounts(node_factory):
    """We steal this from core lightning test_pay.py, and we are try just
    to pay and invoice with amount"""
    l1, l2 = node_factory.line_graph(2, opts=[opts, opts], wait_for_announce=True)
    inv = l2.rpc.invoice(Millisatoshi("123sat"), 'test_pay_amounts', 'description')['bolt11']

    invoice = only_one(l2.rpc.listinvoices('test_pay_amounts')['invoices'])

    assert invoice['amount_msat'] == Millisatoshi(123000)

    result = l1.rpc.call("barqpay", {"invoice": inv})
    assert result["status"] == "complete"
    # l2 is a peer: both strategies find the same zero-fee route and the
    # ranking keeps the first strategy, `direct`.
    assert result["strategy"] == "direct"
    assert result["invoice_type"] == "bolt11"
    assert result["amount_msat"] == 123000
    assert result["fee_msat"] == 0
    assert len(result["route"]) == 1
    assert result["route"][0]["id"] == l2.info["id"]
    assert result["payment_preimage"] is not None

    invoice = only_one(l2.rpc.listinvoices('test_pay_amounts')['invoices'])
    assert invoice['amount_msat'] >= Millisatoshi(123000)
    assert invoice['status'] == 'paid'


def test_pay_without_amounts(node_factory):
    """We are going to test that we can pay an invoice without amount"""
    l1, l2 = node_factory.line_graph(2, opts=[opts, opts], wait_for_announce=True)
    inv = l2.rpc.invoice("any", 'test_pay_amounts', 'description')['bolt11']

    invoice = only_one(l2.rpc.listinvoices('test_pay_amounts')['invoices'])

    # we fail when there is no amount inside the invoice and we do not
    # specify the amount inside the pay command
    with pytest.raises(RpcError):
        l1.rpc.call("barqpay", {"invoice": inv})
    # The legacy parameter name keeps working.
    l1.rpc.call("barqpay", {"bolt11_invoice": inv, "amount_msat": 123000})

    invoice = only_one(l2.rpc.listinvoices('test_pay_amounts')['invoices'])
    assert invoice['status'] == 'paid'


def test_pay_fail_when_there_is_no_channel(node_factory):
    """We make sure that our is not able to pay an invoice when there is no channel"""
    l1 = node_factory.get_node(options=opts)
    l2 = node_factory.get_node(options=opts)

    inv = l2.rpc.invoice(Millisatoshi("123sat"), 'test_pay_amounts', 'description')['bolt11']

    invoice = only_one(l2.rpc.listinvoices('test_pay_amounts')['invoices'])

    assert invoice['amount_msat'] == Millisatoshi(123000)

    # we fail because there is no channel
    with pytest.raises(RpcError, match="No usable channel"):
        l1.rpc.call("barqpay", {"invoice": inv})

    # Sanity check
    invoice = only_one(l2.rpc.listinvoices('test_pay_amounts')['invoices'])
    assert invoice['status'] == 'unpaid'


def test_pay_direct_strategy(node_factory):
    """The direct strategy only pays peers."""
    l1, l2, l3 = node_factory.line_graph(3, opts=[opts, opts, opts], wait_for_announce=True)

    inv = l2.rpc.invoice(Millisatoshi("10sat"), 'direct', 'description')['bolt11']
    result = l1.rpc.call("barqpay", {"invoice": inv, "strategy": "direct"})
    assert result["status"] == "complete"
    assert result["strategy"] == "direct"
    assert only_one(l2.rpc.listinvoices('direct')['invoices'])['status'] == 'paid'

    inv = l3.rpc.invoice(Millisatoshi("10sat"), 'direct', 'description')['bolt11']
    with pytest.raises(RpcError, match="cannot route this payment"):
        l1.rpc.call("barqpay", {"invoice": inv, "strategy": "direct"})


def test_pay_with_ldk_algo(node_factory):
    """Multi-hop payment: LDK must learn the l2 -> l3 channel from the
    Core Lightning gossip_store."""
    l1, l2, l3 = node_factory.line_graph(3, opts=[opts, opts, opts], wait_for_announce=True)

    sync = l1.rpc.call("barqsyncgossip", {})
    assert sync["gossip_store"]["enabled"] is True
    assert sync["channels"] >= 2
    assert sync["nodes"] >= 3

    info = l1.rpc.call("barqrouteinfo", {"dest_pubkey": l3.info["id"], "amount_msat": 123000})
    assert info["strategy"] == "probabilistic"
    assert [hop["id"] for hop in info["route"]] == [l2.info["id"], l3.info["id"]]
    assert info["fee_msat"] > 0

    inv = l3.rpc.invoice("any", 'test_pay_amounts', 'description')['bolt11']

    # we fail when there is no amount inside the invoice and we do not
    # specify the amount inside the pay command
    with pytest.raises(RpcError):
        l1.rpc.call("barqpay", {"invoice": inv, "strategy": "probabilistic"})
    result = l1.rpc.call("barqpay", {"invoice": inv, "strategy": "probabilistic", "amount_msat": 123000})
    assert result["status"] == "complete"
    assert [hop["id"] for hop in result["route"]] == [l2.info["id"], l3.info["id"]]
    assert result["fee_msat"] > 0
    assert result["amount_sent_msat"] == 123000 + result["fee_msat"]

    invoice = only_one(l3.rpc.listinvoices('test_pay_amounts')['invoices'])
    assert invoice['status'] == 'paid'

    # The scorer learnt from the success and the graph survived on disk.
    barq_dir = os.path.join(l1.daemon.lightning_dir, "regtest", "barq")
    assert os.path.exists(os.path.join(barq_dir, "network_graph.regtest.bin"))
    assert os.path.exists(os.path.join(barq_dir, "scorer.regtest.bin"))


def test_pay_bolt12(node_factory):
    """Pay a BOLT12 invoice over a blinded path."""
    l1, l2, l3 = node_factory.line_graph(3, opts=[opts, opts, opts], wait_for_announce=True)

    offer = l3.rpc.offer("10000msat", "bolt12 test")["bolt12"]
    inv = l1.rpc.fetchinvoice(offer)["invoice"]
    decoded = l1.rpc.decode(inv)
    assert decoded["type"] == "bolt12 invoice"
    assert len(decoded["invoice_paths"]) >= 1

    result = l1.rpc.call("barqpay", {"invoice": inv})
    assert result["status"] == "complete"
    assert result["invoice_type"] == "bolt12"
    assert result["amount_msat"] == 10000
    assert result["blinded_hops"] >= 1
    assert result["payment_preimage"] is not None

    paid = [i for i in l3.rpc.listinvoices()["invoices"] if i["status"] == "paid"]
    assert len(paid) == 1
    assert paid[0]["amount_received_msat"] == Millisatoshi(10000)


def test_pay_bolt12_direct_peer(node_factory):
    """BOLT12 to a direct peer."""
    l1, l2 = node_factory.line_graph(2, opts=[opts, opts], wait_for_announce=True)

    offer = l2.rpc.offer("any", "bolt12 direct")["bolt12"]
    inv = l1.rpc.fetchinvoice(offer=offer, amount_msat=5000)["invoice"]
    result = l1.rpc.call("barqpay", {"invoice": inv})
    assert result["status"] == "complete"
    assert result["invoice_type"] == "bolt12"
    paid = [i for i in l2.rpc.listinvoices()["invoices"] if i["status"] == "paid"]
    assert len(paid) == 1


def test_rapid_gossip_sync_not_configured(node_factory):
    """On regtest there is no public RGS server: asking for it must fail
    cleanly, and pointing the option at a bogus URL is reported."""
    l1, l2 = node_factory.line_graph(2, opts=[opts, opts], wait_for_announce=True)
    inv = l2.rpc.invoice(Millisatoshi("1sat"), 'rgs', 'description')['bolt11']
    with pytest.raises(RpcError, match="not configured"):
        l1.rpc.call("barqpay", {"invoice": inv, "use_rapid_gossip_sync": True})

    l3 = node_factory.get_node(options={
        "plugin": barq_binary,
        "barq-rgs-url": "http://127.0.0.1:1/snapshot",
        "barq-sync-interval": 0,
    })
    sync = l3.rpc.call("barqsyncgossip", {})
    assert sync["rapid_gossip_sync"]["enabled"] is True
    assert "error" in sync["rapid_gossip_sync"]


def _barq_pid(node):
    """The pid lightningd logged when it started barq."""
    import re
    line = node.daemon.is_in_log(r"plugin-manager: started\([0-9]+\) .*barq-plugin")
    return int(re.search(r"started\(([0-9]+)\)", line).group(1))


def _alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def test_plugin_exits_with_lightningd(node_factory):
    """barq must not outlive lightningd: cln4rust's read loop spins on a
    closed stdin, so the plugin exits on the shutdown notification."""
    from pyln.testing.utils import wait_for

    node = node_factory.get_node(options=opts)
    pid = _barq_pid(node)
    assert _alive(pid)
    node.stop()
    wait_for(lambda: not _alive(pid), timeout=30)
