# Node experiment dashboard

A small Rust framebuffer dashboard for the Braiins Deck node mode.

It intentionally avoids GTK, SDL, Wayland, a JSON crate, a service framework,
and a graphics dependency. The release binary opens `/dev/fb0`, maps the
existing framebuffer, draws a fixed 5x7 font at a readable 2x scale, refreshes
machine charts every three seconds, and polls Bitcoin Core's loopback RPC every
30 seconds.

The screen shows:

- uptime, load, RAM, swap, SSD mount, and framebuffer geometry;
- compact rolling load, memory, swap, and chain-sync bar charts;
- Bitcoin Core v31.0 chain height, headers, verification state, peers, archive
  state, on-disk size, and resident RAM;
- `STARTING`, `RUNNING`, and `ONLINE` status transitions as the process and RPC
  become available;
- a short raw-transaction submission reminder. It never handles private keys
  and does not submit transactions itself.

The node is expected to use cookie authentication at
`/mnt/bitcoin-node/.cookie` and RPC at `127.0.0.1:8332`.

## Deck framebuffer

The Deck exposes a `600x1280` physical RGB565 framebuffer, while the panel is
used as a `1280x480` landscape display. The dashboard renders into that logical
landscape canvas, then rotates each logical column into a physical scanout row:

```text
physical row    = 1279 - logical x
physical column = logical y
```

The scanout needs only a 1.2 MiB logical RGB565 canvas and a reusable 960-byte
row staging buffer. Four 80-sample bar histories add only 320 bytes. The staged
bulk copy avoids corrupt partial framebuffer writes and keeps the display
readable without a terminal emulator or graphics stack.

## Visual language

The display reuses RetroDeck dashboard colors: `#1c1c1c` surface,
`#303030` controls, `#5f87af` information, `#87af87` healthy state,
`#ffffaf` pending state, `#af8787` fault state, and `#fe6c27` accent.

## Build

Host tests and a release build need only Rust:

```sh
cargo test --locked --release
cargo build --release
```

For the Deck's ARMv7 musl userspace, the repository includes a reproducible
Nix-backed cross-build helper:

```sh
rustup target add armv7-unknown-linux-musleabihf
./scripts/build-arm.sh
```

The resulting binary is static, size-optimized, LTO-linked, built with one
codegen unit and panic abort, and stripped. It has no runtime data files.

## Run on the Deck

Stop the compositor first, then run the binary as root on the console tty:

```sh
/etc/init.d/bmc-compositor stop
/mnt/data/nes-deck/terminal/node-dashboard /dev/fb0
```

The dashboard is read-only with respect to Bitcoin Core. Terminate it with
SIGTERM or SIGINT. Do not unmount the SSD while Bitcoin Core is running.

## Design constraints

- Direct fbdev only. No compositor or terminal emulator.
- No dependencies outside the Rust standard library and Linux C ABI calls for
  `ioctl`, `mmap`, and `munmap`.
- Fixed readable font, one logical canvas, and bounded RPC response reads.
- RPC timeouts are short so a slow or starting node cannot stall the display.
- The dashboard never prints or renders the RPC cookie.
