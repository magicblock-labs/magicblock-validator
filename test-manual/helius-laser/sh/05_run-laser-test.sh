#!/usr/bin/env bash

DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

RUST_LOG=info cargo run --bin helius-laser --manifest-path "$DIR/../Cargo.toml"
