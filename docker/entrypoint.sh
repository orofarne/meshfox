#!/bin/sh
# Creates the home directory (the image runs as any uid, so there is none to
# start with) and becomes meshfox, so that signals from tini reach it.
set -e
mkdir -p "$HOME"
exec meshfox "$@"
