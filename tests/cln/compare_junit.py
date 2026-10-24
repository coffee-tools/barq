#!/usr/bin/env python3
"""Compare a native and a barq run of Core Lightning's test suite.

    compare_junit.py --native n1.xml [n2.xml...] --barq b1.xml [b2.xml...]
                     [--expected expected-failures.txt] [--markdown summary.md]

A test that passes natively but fails with barq is a barq gap. One that fails
both ways says nothing about barq. `--expected` takes the list of tests barq
is known not to pass yet (one test id per line, `# reason` allowed); the exit
status is 1 when barq fails a test that passes natively and is not expected.
"""
import argparse
import sys
import xml.etree.ElementTree as ET


def results(paths):
    """test id -> passed | failed | skipped, over every junit file given"""
    out = {}
    cases = [c for path in paths for c in ET.parse(path).getroot().iter("testcase")]
    for case in cases:
        name = f"{case.get('classname', '').split('.')[-1]}::{case.get('name')}"
        if case.find("failure") is not None or case.find("error") is not None:
            status = "failed"
        elif case.find("skipped") is not None:
            status = "skipped"
        else:
            status = "passed"
        # A rerun (pytest-rerunfailures) appears twice: keep the best outcome.
        if out.get(name) != "passed":
            out[name] = status
    return out


def expected_failures(path):
    if not path:
        return {}
    out = {}
    for line in open(path):
        test, _, reason = line.partition("#")
        if test.strip():
            out[test.strip()] = reason.strip()
    return out


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--native", nargs="+", required=True)
    p.add_argument("--barq", nargs="+", required=True)
    p.add_argument("--markdown")
    p.add_argument("--expected")
    args = p.parse_args()

    native, barq = results(args.native), results(args.barq)
    expected = expected_failures(args.expected)
    tests = sorted(set(native) | set(barq))
    gaps = [t for t in tests if native.get(t) == "passed" and barq.get(t) == "failed"]
    both = [t for t in tests if native.get(t) == "failed" and barq.get(t) == "failed"]
    fixed = [t for t in tests if native.get(t) == "failed" and barq.get(t) == "passed"]
    unexpected = [t for t in gaps if t not in expected]
    newly_passing = [t for t in expected if barq.get(t) == "passed"]

    def count(r, s):
        return sum(1 for v in r.values() if v == s)

    lines = [
        "## barq as `pay`: Core Lightning test suite",
        "",
        "| | passed | failed | skipped |",
        "| --- | --- | --- | --- |",
        f"| native | {count(native, 'passed')} | {count(native, 'failed')} | {count(native, 'skipped')} |",
        f"| barq | {count(barq, 'passed')} | {count(barq, 'failed')} | {count(barq, 'skipped')} |",
        "",
        f"- **barq gaps** (pass natively, fail with barq): {len(gaps)}, of which {len(unexpected)} not in the expected list",
        f"- **fail both ways** (not barq related): {len(both)}",
        f"- **pass only with barq**: {len(fixed)}",
        f"- **expected failures now passing** (remove them from the list): {len(newly_passing)}",
        "",
    ]
    for title, items in [("Unexpected barq gaps", unexpected),
                         ("Expected barq gaps", [t for t in gaps if t in expected]),
                         ("Fail both ways", both),
                         ("Expected failures now passing", newly_passing)]:
        if items:
            lines += [f"### {title}", ""]
            lines += [f"- `{t}`" + (f": {expected[t]}" if t in expected else "") for t in items]
            lines.append("")
    report = "\n".join(lines)
    print(report)
    if args.markdown:
        with open(args.markdown, "a") as f:
            f.write(report + "\n")
    return 1 if unexpected else 0


if __name__ == "__main__":
    sys.exit(main())
