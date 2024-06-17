import os

from pyln.testing.fixtures import *  # noqa: F403

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
