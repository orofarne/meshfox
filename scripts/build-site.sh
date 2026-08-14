#!/usr/bin/env bash
# Builds the static meshfox.orofarne.net site. This is the Cloudflare Pages
# "Build command" — instead of compiling meshfox-cli from source (Pages has
# no Rust preinstalled and no cross-build cache between runs), it grabs the
# latest tagged GitHub release's prebuilt Linux x86_64 binary, which is what
# Pages' build containers run on.
set -euo pipefail

curl -fsSL -o /tmp/meshfox.tar.gz "https://github.com/orofarne/meshfox/releases/latest/download/meshfox-linux-x86_64.tar.gz"
tar xzf /tmp/meshfox.tar.gz -C /tmp
/tmp/meshfox-linux-x86_64/meshfox static README.md --template ./site-template --out ./site-dist --force
