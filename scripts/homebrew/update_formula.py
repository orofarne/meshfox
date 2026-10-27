"""Generate the tap formula from all four published, versioned release archives."""

import argparse
import hashlib
from pathlib import Path
import re


TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
)
VERSION = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"


def update_formula(tag: str, archives: Path, formula: Path) -> bool:
    match = re.fullmatch("v" + VERSION, tag)
    if not match:
        raise ValueError(f"Expected a stable vX.Y.Z release tag, got {tag!r}")
    version = tuple(map(int, match.groups()))
    if formula.exists():
        current = re.search(r'^  version "(' + VERSION + r')"$', formula.read_text(), re.M)
        if not current:
            raise ValueError("Cannot determine the existing tap formula's version")
        if tuple(map(int, current.groups()[1:])) > version:
            print(f"Keeping newer tap version {current[1]}; skipping {tag}")
            return False

    content = Path(__file__).with_name("meshfox.rb.in").read_text()
    content = content.replace("@VERSION@", tag[1:])
    # Read every archive before writing, so missing downloads cannot publish
    # a partially updated formula. Hash actual published bytes, not build outputs.
    for target in TARGETS:
        archive = archives / f"meshfox-{target}.tar.gz"
        digest = hashlib.sha256()
        with archive.open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
        content = content.replace(f"@SHA256_{target}@", digest.hexdigest())

    if formula.exists() and formula.read_text() == content:
        print(f"Tap formula already matches {tag}")
        return False
    formula.parent.mkdir(parents=True, exist_ok=True)
    formula.write_text(content)
    print(f"Generated {formula} for {tag}")
    return True


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("archives", type=Path)
    parser.add_argument("formula", type=Path)
    args = parser.parse_args()
    update_formula(args.tag, args.archives, args.formula)
