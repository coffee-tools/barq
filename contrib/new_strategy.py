#!/usr/bin/env python3
"""Generate the skeleton of a new barq routing strategy (issue #14).

    contrib/new_strategy.py NAME --lang rust     # a Rust strategy in barq-common
    contrib/new_strategy.py NAME --lang python   # an external strategy program

Rust: writes barq-common/src/algorithms/NAME.rs (a `Strategy` with a unit
test on a simulated network) and registers the module.
Python: writes NAME.py (or --out), an executable that speaks barq's external
strategy protocol; run barq with `barq-external-strategy=/path/to/NAME.py`.
"""
import argparse
import os
import re
import stat
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TEMPLATES = os.path.join(ROOT, "contrib", "templates")


def render(template, name):
    with open(os.path.join(TEMPLATES, template)) as f:
        text = f.read()
    type_name = "".join(part.capitalize() for part in name.split("_"))
    return (text.replace("__NAME__", name)
                .replace("__MODULE__", name)
                .replace("__TYPE__", type_name))


def write(path, text, executable=False, force=False):
    if os.path.exists(path) and not force:
        sys.exit(f"{path} already exists (use --force to overwrite)")
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)
    if executable:
        mode = os.stat(path).st_mode
        os.chmod(path, mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    print(f"wrote {path}")


def rust(name, root, force):
    algorithms = os.path.join(root, "barq-common", "src", "algorithms")
    write(os.path.join(algorithms, f"{name}.rs"), render("strategy.rs", name), force=force)
    mod_rs = os.path.join(algorithms, "mod.rs")
    with open(mod_rs) as f:
        modules = f.read()
    line = f"pub mod {name};"
    if line not in modules.split("\n"):
        with open(mod_rs, "a") as f:
            f.write(("" if modules.endswith("\n") else "\n") + line + "\n")
        print(f"registered `{line}` in {mod_rs}")
    print(f"next: cargo test -p barq-common {name}")


def python(name, out, force):
    path = out or f"{name}.py"
    write(path, render("external_strategy.py", name), executable=True, force=force)
    print(f"next: lightningd ... --barq-external-strategy={os.path.abspath(path)}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("name", help="snake_case strategy name")
    parser.add_argument("--lang", choices=["rust", "python"], default="rust")
    parser.add_argument("--out", help="output path (python only)")
    parser.add_argument("--root", default=ROOT, help="barq checkout (rust only)")
    parser.add_argument("--force", action="store_true", help="overwrite existing files")
    args = parser.parse_args()
    if not re.fullmatch(r"[a-z][a-z0-9_]*", args.name):
        sys.exit("the name must be snake_case: lowercase letters, digits and _")
    if args.lang == "rust":
        rust(args.name, args.root, args.force)
    else:
        python(args.name, args.out, args.force)


if __name__ == "__main__":
    main()
