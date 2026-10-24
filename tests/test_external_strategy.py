"""Strategies in any language (issue #12) and the strategy skeleton
generator (issue #14)."""
import os
import shutil
import subprocess
import sys

import pytest

from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import only_one
from pyln.client import RpcError

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
barq_binary = os.path.join(ROOT, "target/debug/barq-plugin")
generator = os.path.join(ROOT, "contrib/new_strategy.py")


def generate(*args, cwd=None):
    return subprocess.run(
        [sys.executable, generator, *args],
        cwd=cwd, capture_output=True, text=True,
    )


def write_script(path, body):
    path.write_text("#!/bin/sh\n" + body + "\n")
    path.chmod(0o755)
    return str(path)


def network(node_factory, strategy_program):
    opts = {"plugin": barq_binary, "barq-external-strategy": strategy_program}
    return node_factory.line_graph(
        3, opts=[opts, {"plugin": barq_binary}, {"plugin": barq_binary}], wait_for_announce=True
    )


def test_generated_python_strategy_pays_over_two_hops(node_factory, tmp_path):
    """#14 generates a Python strategy; #12 runs it. The template finds the
    path over `listchannels` and computes the fees itself."""
    out = tmp_path / "fewest_hops.py"
    result = generate("fewest_hops", "--lang", "python", "--out", str(out))
    assert result.returncode == 0, result.stderr
    assert os.access(out, os.X_OK)

    l1, l2, l3 = network(node_factory, str(out))
    assert l1.daemon.is_in_log(f"external strategy: {out}")
    amount = 123_000

    info = l1.rpc.call("barqrouteinfo", {
        "dest_pubkey": l3.info["id"], "amount_msat": amount, "strategy": "external",
    })
    assert info["strategy"] == "external"
    assert [h["id"] for h in info["route"]] == [l2.info["id"], l3.info["id"]]
    # The external fee matches l2's policy, like LDK's.
    ldk = l1.rpc.call("barqrouteinfo", {
        "dest_pubkey": l3.info["id"], "amount_msat": amount, "strategy": "probabilistic",
    })
    assert info["fee_msat"] == ldk["fee_msat"] > 0

    inv = l3.rpc.invoice(amount, "external", "external")["bolt11"]
    paid = l1.rpc.call("barqpay", {"invoice": inv, "strategy": "external"})
    assert paid["status"] == "complete"
    assert paid["strategy"] == "external"
    assert only_one(l3.rpc.listinvoices("external")["invoices"])["status"] == "paid"
    record = l1.rpc.call("barqinspect", {"id": paid["id"]})
    assert record["attempts"][0]["strategy"] == "external"


def test_external_strategy_takes_part_in_ranking(node_factory, tmp_path):
    """A configured external strategy is ranked with the built-in ones; when
    it fails, the others still pay."""
    broken = write_script(tmp_path / "broken.sh", 'cat > /dev/null; echo \'{"error": "no idea"}\'')
    l1, l2, l3 = network(node_factory, broken)

    info = l1.rpc.call("barqrouteinfo", {"dest_pubkey": l3.info["id"], "amount_msat": 10_000})
    assert info["strategy"] == "probabilistic"
    assert {"strategy": "external", "reason": "no idea"} in info["rejected"]

    inv = l3.rpc.invoice(10_000, "auto", "auto")["bolt11"]
    assert l1.rpc.call("barqpay", {"invoice": inv})["status"] == "complete"


def test_invalid_external_routes_are_rejected(node_factory, tmp_path):
    """barq checks the route before sending: here the strategy claims l2 is
    the destination."""
    holder = {}
    program = tmp_path / "liar.sh"
    write_script(program, "cat > /dev/null; cat " + str(tmp_path / "answer.json"))
    l1, l2, l3 = network(node_factory, str(program))
    channel = only_one(l1.rpc.listpeerchannels(l2.info["id"])["channels"])["short_channel_id"]
    holder["answer"] = (
        '{"route": [{"id": "%s", "channel": "%s", "fee_msat": 10000, "cltv_expiry_delta": 18}]}'
        % (l2.info["id"], channel)
    )
    (tmp_path / "answer.json").write_text(holder["answer"])

    inv = l3.rpc.invoice(10_000, "liar", "liar")["bolt11"]
    with pytest.raises(RpcError, match="does not end at the destination"):
        l1.rpc.call("barqpay", {"invoice": inv, "strategy": "external"})
    payment_hash = l1.rpc.decode(inv)["payment_hash"]
    assert l1.rpc.listsendpays(payment_hash=payment_hash)["payments"] == []


def test_external_strategy_must_be_configured(node_factory):
    l1, l2 = node_factory.line_graph(2, opts=[{"plugin": barq_binary}] * 2, wait_for_announce=True)
    inv = l2.rpc.invoice(1_000, "none", "none")["bolt11"]
    with pytest.raises(RpcError, match="barq-external-strategy"):
        l1.rpc.call("barqpay", {"invoice": inv, "strategy": "external"})


def test_generator_validates_names(tmp_path):
    assert generate("Not-Snake").returncode != 0
    out = tmp_path / "dup.py"
    assert generate("dup", "--lang", "python", "--out", str(out)).returncode == 0
    second = generate("dup", "--lang", "python", "--out", str(out))
    assert second.returncode != 0 and "already exists" in second.stderr
    assert generate("dup", "--lang", "python", "--out", str(out), "--force").returncode == 0


def test_generated_rust_strategy_compiles_and_passes_its_test(tmp_path):
    """#14: the Rust skeleton is registered in barq-common, compiles, and its
    generated unit test passes. Runs on a copy of the repository."""
    copy = tmp_path / "barq"
    files = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard"],
        cwd=ROOT, capture_output=True, text=True, check=True,
    ).stdout.split()
    for name in files:
        source = os.path.join(ROOT, name)
        if not os.path.isfile(source):
            continue
        target = copy / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)

    result = generate("cheapest_peer", "--lang", "rust", "--root", str(copy))
    assert result.returncode == 0, result.stderr
    module = copy / "barq-common/src/algorithms/cheapest_peer.rs"
    assert module.exists()
    assert "pub mod cheapest_peer;" in (copy / "barq-common/src/algorithms/mod.rs").read_text()

    env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(ROOT, "target", "skeleton-test"))
    test = subprocess.run(
        ["cargo", "test", "-p", "barq-common", "cheapest_peer"],
        cwd=copy, capture_output=True, text=True, env=env, timeout=1800,
    )
    assert test.returncode == 0, test.stdout[-3000:] + test.stderr[-3000:]
    assert "cheapest_peer_routes_to_a_peer ... ok" in test.stdout
