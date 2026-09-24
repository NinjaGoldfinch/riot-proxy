#!/usr/bin/env python3
"""Compare the operations of two OpenAPI documents (plan P4-03, P4 exit check).

v1's document has no operationIds, so an operation is identified by
"METHOD /path". Exits 1 if any operation in OLD (under the given prefixes) is
missing from NEW.

    scripts/compare-openapi.py docs/contract/v1-openapi.json openapi.json \\
        --prefix /v1/riot/ --prefix /v1/lol/
"""

import argparse
import json
import sys

METHODS = ("get", "put", "post", "delete", "patch", "head", "options")


def operations(path, prefixes):
    with open(path, encoding="utf-8") as f:
        doc = json.load(f)
    found = set()
    for route, item in doc.get("paths", {}).items():
        if prefixes and not any(route.startswith(p) for p in prefixes):
            continue
        for method in METHODS:
            if method in item:
                found.add(f"{method.upper()} {route}")
    return found


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("old", help="the contract, e.g. docs/contract/v1-openapi.json")
    parser.add_argument("new", help="the document under test, e.g. the output of `riot-proxy spec`")
    parser.add_argument("--prefix", action="append", default=[], help="only compare paths with this prefix (repeatable)")
    args = parser.parse_args(argv)

    old = operations(args.old, args.prefix)
    new = operations(args.new, args.prefix)
    missing = sorted(old - new)
    added = sorted(new - old)

    scope = ", ".join(args.prefix) if args.prefix else "all paths"
    print(f"compared {len(old)} contract operations ({scope})")
    for op in missing:
        print(f"missing  {op}")
    for op in added:
        print(f"added    {op}")
    print(f"{len(missing)} missing, {len(added)} added")
    return 1 if missing else 0


if __name__ == "__main__":
    sys.exit(main())
