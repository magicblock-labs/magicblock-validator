#!/usr/bin/env bash

DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"

RUST_LOG=warn,magicblock=info,magicblock_chainlink=debug,magicblock_chainlink::fetch_cloner=trace,magicblock_accounts=debug \
			cargo run --bin magicblock-validator --manifest-path=$DIR/../../../Cargo.toml \
			-- /tmp/mb-test-laser.toml
