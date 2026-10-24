#!/usr/bin/env python3
"""Test plugin: hold the HTLCs of chosen payments until released.

Works like a hold invoice: `holdhtlcs <payment_hash>` makes this node keep
the HTLCs paying that hash pending, `releasehtlcs <payment_hash>` lets them
through. Other HTLCs are not held, so payments of other invoices complete.
"""
from threading import Lock

from pyln.client import Plugin

plugin = Plugin()
lock = Lock()
hold = set()
# payment_hash -> htlc_accepted requests waiting for an answer
held = {}


@plugin.method("holdhtlcs")
def holdhtlcs(plugin, payment_hash):
    with lock:
        hold.add(payment_hash)
    return {}


@plugin.method("heldhtlcs")
def heldhtlcs(plugin):
    with lock:
        return {"held": {h: len(requests) for h, requests in held.items()}}


@plugin.method("releasehtlcs")
def releasehtlcs(plugin, payment_hash):
    with lock:
        hold.discard(payment_hash)
        requests = held.pop(payment_hash, [])
    for request in requests:
        request.set_result({"result": "continue"})
    return {"released": len(requests)}


@plugin.async_hook("htlc_accepted")
def on_htlc_accepted(onion, htlc, plugin, request, **kwargs):
    with lock:
        if htlc["payment_hash"] in hold:
            plugin.log(f"holding the HTLC of {htlc['payment_hash']}")
            held.setdefault(htlc["payment_hash"], []).append(request)
            return
    request.set_result({"result": "continue"})


plugin.run()
