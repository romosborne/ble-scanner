#!/bin/bash
set -o errexit
set -o nounset
set -o pipefail
set -o xtrace

readonly TARGET_ARCH=armv7-unknown-linux-gnueabihf
readonly LINK_FLAGS='-L /usr/armv7-linux-gnueabihf/lib/ -L /usr/lib/armv7-linux-gnueabihf/'

RUSTFLAGS=${LINK_FLAGS} cross build --release --target=${TARGET_ARCH}
