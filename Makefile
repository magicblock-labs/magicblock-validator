DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

CARGO_TEST=nextest run
CARGO_TEST_NOCAP=nextest run --nocapture
$(if $(shell command -v cargo-nextest 2> /dev/null),,$(eval CARGO_TEST=test))
$(if $(shell command -v cargo-nextest 2> /dev/null),,$(eval CARGO_TEST_NOCAP=test -- --nocapture))

test:
	RUST_BACKTRACE=1 cargo $(CARGO_TEST) && \
	$(MAKE) -C $(DIR)/test-integration test

test-log:
	cargo $(CARGO_TEST_NOCAP)

test-bank:
	cargo $(CARGO_TEST_NOCAP) --package magicblock-bank

list:
	@LC_ALL=C $(MAKE) -pRrq -f $(firstword $(MAKEFILE_LIST)) : 2>/dev/null | awk -v RS= -F: '/(^|\n)# Files(\n|$$)/,/(^|\n)# Finished Make data base/ {if ($$1 !~ "^[#.]") {print $$1}}' | sort | egrep -v -e '^[^[:alnum:]]' -e '^$@$$'

ex-clone-custom:
	cargo run --package=magicblock-mutator --example clone_solx_custom

ex-rpc:
	cargo run --package=magicblock-rpc --example rpc

ex-rpc-release:
	cargo run --release --package=magicblock-rpc --example rpc

run-release:
	cargo run --release

run-release-no-geyser-cache:
	GEYSER_CACHE_DISABLE=accounts,transactions \
	cargo run --release

run-release-no-geyser:
	GEYSER_DISABLE=accounts,transactions \
	cargo run --release

update-sysvars:
	$(DIR)/test-integration/sysvars/sh/update

fmt:
	cargo +nightly fmt -- --config-path rustfmt-nightly.toml

# TODO - use "-W clippy::pedantic"
lint:
	cargo clippy --all-targets -- -D warnings

ci-test-unit:
	RUST_BACKTRACE=1 cargo $(CARGO_TEST_NOCAP)

ci-test-integration:
	cargo build --locked && \
	$(MAKE) -C $(DIR)/test-integration test

## NOTE: We're getting the following error in github CI when trying to use
#  nightly Rust. Until that is fixed we have to use stable to verify format.
#
#  error: failed to install component: 'rustc-x86_64-unknown-linux-gnu', detected conflict: 'lib/rustlib/x86_64-unknown-linux-gnu/bin/llc'
#
#  However this should not be a problem as our formatting rules for nightly
#  are more strict than the non-nightly ones.
ci-fmt:
	cargo +nightly fmt --check -- --config-path rustfmt-nightly.toml

ci-lint: lint

## Changing the Rust config causes everything to rebuild
## In order to avoid that add the below inside a <workspace-root>/.cargo/config.toml
# ```
# [build]
# rustflags = ["--cfg", "tokio_unstable"]
# ```
tokio-console:
	RUSTFLAGS="--cfg tokio_unstable" cargo run --release --features=tokio-console

.PHONY:
	list test test-log test-bank fmt lint ex-clone-custom ex-rpc ex-rpc-release tokio-console
