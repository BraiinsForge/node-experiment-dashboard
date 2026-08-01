# Node experiment dashboard

A small Rust framebuffer dashboard for the Braiins Deck node mode.

It intentionally avoids GTK, SDL, Wayland, a JSON crate, a service framework,
and a graphics dependency. The release binary opens `/dev/fb0`, maps the
existing framebuffer, draws a small fixed 5x7 font, and polls Bitcoin Core's
loopback RPC every two seconds.

The screen shows:

- uptime, load, RAM, swap, SSD mount, and framebuffer geometry;
- Bitcoin Core v31.0 chain height, headers, verification state, peers, archive
  state, and on-disk size;
- a short raw-transaction submission reminder. It never handles private keys
  and does not submit transactions itself.

The node is expected to use cookie authentication at
`/mnt/bitcoin-node/.cookie` and RPC at `127.0.0.1:8332`.

## Build

Host tests and a release build need only Rust:

```sh
cargo test --release
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

- Direct fbdev only. No compositor and no framebuffer-sized backbuffer.
- No dependencies outside the Rust standard library and Linux C ABI calls for
  `ioctl`, `mmap`, and `munmap`.
- Fixed small font and bounded RPC response reads.
- RPC timeouts are short so a slow or starting node cannot stall the display.
- The dashboard never prints or renders the RPC cookie.
