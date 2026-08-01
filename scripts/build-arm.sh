#!/bin/sh
set -eu

cc_dir=${CC_DIR:-$(nix eval --raw 'nixpkgs#legacyPackages.x86_64-linux.pkgsCross.armv7l-hf-multiplatform.pkgsStatic.stdenv.cc.outPath')}
cc="$cc_dir/bin/armv7l-unknown-linux-musleabihf-gcc"

exec env \
  CC="$cc" \
  CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER="$cc" \
  cargo build --locked --release --target armv7-unknown-linux-musleabihf
