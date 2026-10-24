# Core Lightning's tests with barq as `pay`

barq aims to be a drop-in replacement for Core Lightning's `pay`. The best
proof is Core Lightning's own test suite: run it once as is and once with
barq serving `pay` on every node, and compare. A test that passes natively
and fails with barq is a place where barq behaves differently from `pay`.

The tests are not copied here: they need the Core Lightning tree they come
from (its `lightningd` debug build, test plugins and fixtures). Instead this
directory pins the version and carries what is added to it:

| File | What it is |
| --- | --- |
| `CLN_VERSION` | the Core Lightning tag the tests come from |
| `fixtures-barq.patch` | the `TEST_BARQ_PLUGIN` hook for Core Lightning's `tests/fixtures.py` |
| `expected-failures.txt` | tests barq is known not to pass yet, each with the reason |
| `compare_junit.py` | compares the native and barq junit reports |
| `run.sh` | runs everything locally |

## The hook

With `TEST_BARQ_PLUGIN=/path/to/barq-plugin`, every test node starts with
barq and `barq-override-pay=true`. xpay's own takeover of `pay` is turned
off with `xpay-handle-pay=false`, because two plugins answering the same
`rpc_command` crash `lightningd`.

## Running it locally

```bash
tests/cln/run.sh tests/test_pay.py tests/test_xpay.py
```

It clones Core Lightning at `CLN_VERSION` into `tests/cln/lightning`,
applies the patch, builds both projects, runs the tests twice and prints the
comparison. Core Lightning's build dependencies must be installed first.

## CI

The workflow lives in a Core Lightning fork,
[vincenzopalazzo/lightning@barq-pay-tests](https://github.com/vincenzopalazzo/lightning/tree/barq-pay-tests),
which is the tag in `CLN_VERSION` plus the patch. It builds Core Lightning
once, runs the tests natively and with barq in parallel groups, and writes the
comparison to the run summary. barq's `cln-test-suite.yml` calls it for every
pull request with that pull request's commit, so the expected-failures list
is always read from the barq commit under test.

## Moving to a new Core Lightning version

1. Update `CLN_VERSION`, and rebase the fork branch onto the new tag.
2. Refresh the patch if `tests/fixtures.py` changed:
   `git -C tests/cln/lightning diff tests/fixtures.py > tests/cln/fixtures-barq.patch`.
3. Run the suite and update `expected-failures.txt` from the comparison.
