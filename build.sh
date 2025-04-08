#!/bin/bash

RUSTFLAGS='-C target-feature=+crt-static' cargo build --release --target x86_64-unknown-linux-musl
# RUSTFLAGS='-C target-feature=+crt-static' cargo build --release --target aarch64-unknown-linux-musl