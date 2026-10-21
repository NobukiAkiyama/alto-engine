#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
sh scripts/build-m0.sh
pnpm --dir packages/alto-sdk install --frozen-lockfile
pnpm --dir packages/alto-sdk build
