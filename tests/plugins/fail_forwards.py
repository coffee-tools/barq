#!/usr/bin/env python3
"""Test plugin: fail every HTLC this node is asked to forward.

Used to make a path fail in the middle of the route, deterministically.
Payments to this node itself are accepted.
"""
from pyln.client import Plugin

plugin = Plugin()


@plugin.hook("htlc_accepted")
def on_htlc_accepted(onion, htlc, plugin, **kwargs):
    if "short_channel_id" in onion:
        plugin.log(f"failing forward to {onion['short_channel_id']}")
        # temporary_node_failure (NODE | 2)
        return {"result": "fail", "failure_message": "2002"}
    return {"result": "continue"}


plugin.run()
