#!/usr/bin/env bash
# Builds the static meshfox.orofarne.net site. This is the Cloudflare Pages
# "Build command" — instead of compiling meshfox-cli from source (Pages has
# no Rust preinstalled and no cross-build cache between runs), it grabs the
# latest tagged GitHub release's prebuilt Linux x86_64 binary, which is what
# Pages' build containers run on.
set -euo pipefail

# Cloudflare Pages checks out a *shallow* clone (one commit) by default —
# confirmed community-known behavior, no per-project setting to turn it off.
# `--sitemap-git-dates` below needs each canvas's own real last-commit date
# (`git log -1 --format=%cI -- <path>`), not just whatever single commit
# Pages happened to build from — a shallow clone would silently give every
# page the *same*, wrong `<lastmod>` (the build commit's date) instead of
# erroring, so this has to run before `meshfox static`, not be discovered
# missing after. `--unshallow` on an already-full clone (e.g. a local run of
# this script) is a documented no-op error from git itself — `|| true`
# swallows exactly that, nothing else (the actual fetch's own network/auth
# failures still surface, since they happen before that exit).
if [ "$(git rev-parse --is-shallow-repository)" = "true" ]; then
    git fetch --unshallow || true
fi

curl -fsSL -o /tmp/meshfox.tar.gz "https://github.com/orofarne/meshfox/releases/latest/download/meshfox-x86_64-unknown-linux-gnu.tar.gz"
tar xzf /tmp/meshfox.tar.gz -C /tmp
/tmp/x86_64-unknown-linux-gnu/meshfox static README.md --template ./site-template --out ./site-dist --force \
    --copy-files --recursive --sitemap --sitemap-git-dates
