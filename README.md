# Node experiment dashboard

Small Rust fbdev dashboard for the Braiins Deck Bitcoin node.

- Logical `1280x480` display rotated into the Deck's `600x1280` RGB565 framebuffer.
- Shows machine load/RAM/swap, Core status/RAM/sync, and bounded mempool stats.
- System charts refresh every 3 seconds; Core RPC is polled every 30 seconds.
- Uses the RetroDeck palette. No GUI, terminal emulator, or Rust dependencies.

## Build

```sh
cargo test --locked --release
./scripts/build-arm.sh
```

## Run on the Deck

```sh
/etc/init.d/bmc-compositor stop
/mnt/bitcoin-node/runtime/node-dashboard /dev/fb0
```

The dashboard is read-only. It uses Bitcoin Core cookie authentication at
`/mnt/bitcoin-node/.cookie` and never renders it.
