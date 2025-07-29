#!/bin/bash

ARCH=$(uname -m)

case $ARCH in
    x86_64)
        TARGET="x86_64-unknown-linux-musl"
        ;;
    aarch64)
        TARGET="aarch64-unknown-linux-musl"
        ;;
    *)
        echo "不支持的架构: $ARCH"
        exit 1
        ;;
esac

RUSTFLAGS='-C target-feature=+crt-static' cargo build --release --target $TARGET