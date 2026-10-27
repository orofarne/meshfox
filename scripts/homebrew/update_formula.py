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


def contained_path(path: Path, base: Path) -> Path:
    """Resolve '..' and symlinks before enforcing a trusted filesystem root."""
    resolved = path.resolve()
    if not resolved.is_relative_to(base):
        raise ValueError(f"Path {path} resolves outside allowed root {base}")
    return resolved


def update_formula(tag: str, archives: Path, formula: Path) -> bool:
    match = re.fullmatch("v" + VERSION, tag)
    if not match:
        raise ValueError(f"Expected a stable vX.Y.Z release tag, got {tag!r}")
    version = tuple(map(int, match.groups()))
    # These roots match homebrew.yml's checkout and download destinations.
    # Derive them from the working directory, never from CLI-controlled parents.
    workspace = Path.cwd().resolve()
    archives_base = contained_path(workspace / "release-archives", workspace)
    tap_base = contained_path(workspace / "homebrew-tap", workspace)
    archives_dir = contained_path(archives, archives_base)
    requested_formula = contained_path(formula, tap_base)
    formula_path = contained_path(tap_base / "Formula" / "meshfox.rb", tap_base)
    if formula.name != "meshfox.rb" or requested_formula != formula_path:
        raise ValueError("Formula must be homebrew-tap/Formula/meshfox.rb")

    if formula_path.exists():
        current = re.search(r'^  version "(' + VERSION + r')"$', formula_path.read_text(), re.M)
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
        archive = contained_path(archives_dir / f"meshfox-{target}.tar.gz", archives_dir)
        digest = hashlib.sha256()
        with archive.open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
        content = content.replace(f"@SHA256_{target}@", digest.hexdigest())

    if formula_path.exists() and formula_path.read_text() == content:
        print(f"Tap formula already matches {tag}")
        return False
    # Sonar's taint analysis (pythonsecurity:S2083, "path traversal") flags the
    # write below; it is marked a false positive in SonarCloud. Why it is safe:
    # `formula_path` is built from the working directory and constants
    # ("homebrew-tap/Formula/meshfox.rb"), never from the `formula` argument
    # (which is only compared against it and rejected on any difference), and
    # `contained_path` resolved symlinks and '..' and checked it lies under
    # `tap_base`. `content` is the template with a validated vX.Y.Z version
    # and hex digests substituted in; it has no say in where it is written.
    #
    # Keep the next line's text exactly as it is: inline suppression comments
    # do not apply to taint findings, and SonarCloud matches the
    # false-positive mark to the line by its text, so editing the line (a
    # trailing comment included) makes it a new, open issue.
    formula_path.parent.mkdir(parents=True, exist_ok=True)
    formula_path.write_text(content)
    print(f"Generated {formula_path} for {tag}")
    return True


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("archives", type=Path)
    parser.add_argument("formula", type=Path)
    args = parser.parse_args()
    update_formula(args.tag, args.archives, args.formula)
