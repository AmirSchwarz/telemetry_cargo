#!/usr/bin/env python3
"""Vendor canonical registry/fixtures into the self-contained Rust crate."""
import argparse
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail on stale generated assets")
    args = parser.parse_args()
    source = Path(__file__).resolve().parent
    target = source.parent / "crates" / "telemetry-schema" / "schema"
    pairs = [(source / "registry-v1.json", target / "registry-v1.json"),
             (source / "fixtures" / "action-v1.json", target / "action-v1.json"),
             (source / "fixtures" / "action-v1.json",
              source.parent / "crates" / "telemetry-queue" / "schema" / "action-v1.json")]
    stale = []
    for original, generated in pairs:
        contents = original.read_bytes()
        if not generated.exists() or generated.read_bytes() != contents:
            if args.check:
                stale.append(str(generated))
            else:
                generated.parent.mkdir(parents=True, exist_ok=True)
                generated.write_bytes(contents)
    if stale:
        parser.exit(1, "stale generated schema assets; run python3 schemas/generate.py\n")
    print("Schema assets are current.")


if __name__ == "__main__":
    main()
