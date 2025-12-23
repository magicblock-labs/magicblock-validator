#!/usr/bin/env bash

RUST_LOG=warn,magicblock=info,magicblock_chainlink=debug,magicblock_chainlink::fetch_cloner=trace,magicblock_accounts=debug \
			cargo run --bin magicblock-validator \
			-- /tmp/mb-test-laser.toml
