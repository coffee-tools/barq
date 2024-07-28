import os

from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import only_one
from pyln.client import Millisatoshi

barq_binary = os.path.join(os.path.dirname(__file__), "../target/debug/barq-plugin")


def test_init(node_factory):
    """This is just a monkey test to check that the
    test env is configured fine"""
    node = node_factory.get_node(
        options={
            "plugin": barq_binary,
        }
    )
    assert True


def test_pay_amounts(node_factory):
    """We steal this from core lightning test_pay.py, and we are try just 
    to pay and invoice with amount"""
    l1, l2 = node_factory.line_graph(2, opts=[{"plugin": barq_binary }, { "plugin": barq_binary}], wait_for_announce=True)
    inv = l2.rpc.invoice(Millisatoshi("123sat"), 'test_pay_amounts', 'description')['bolt11']

    invoice = only_one(l2.rpc.listinvoices('test_pay_amounts')['invoices'])

    assert invoice['amount_msat'] == Millisatoshi(123000)

    l1.rpc.call("barqpay", {"bolt11_invoice": inv})

    invoice = only_one(l2.rpc.listinvoices('test_pay_amounts')['invoices'])
    assert invoice['amount_received_msat'] >= Millisatoshi(123000)


def test_pay_without_amounts(node_factory):
    """We are going to test that we can pay an invoice without amount"""
    l1, l2 = node_factory.line_graph(2, opts=[{"plugin": barq_binary }, { "plugin": barq_binary}], wait_for_announce=True)
    inv = l2.rpc.invoice("any", 'test_pay_amounts', 'description')['bolt11']

    invoice = only_one(l2.rpc.listinvoices('test_pay_amounts')['invoices'])

    l1.rpc.call("barqpay", {"bolt11_invoice": inv, "amount_msat": 123000})

    invoice = only_one(l2.rpc.listinvoices('test_pay_amounts')['invoices'])
    assert invoice['amount_received_msat'] >= Millisatoshi(123000)