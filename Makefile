DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

CARGO_TEST=nextest run
CARGO_TEST_NOCAP=nextest run --nocapture
$(if $(shell command -v cargo-nextest 2> /dev/null),,$(eval CARGO_TEST=test))
$(if $(shell command -v cargo-nextest 2> /dev/null),,$(eval CARGO_TEST_NOCAP=test -- --nocapture))

test:
	cargo $(CARGO_TEST)

test-log:
	cargo $(CARGO_TEST_NOCAP)

test-bank:
	cargo $(CARGO_TEST_NOCAP) --package sleipnir-bank

list:
	@LC_ALL=C $(MAKE) -pRrq -f $(firstword $(MAKEFILE_LIST)) : 2>/dev/null | awk -v RS= -F: '/(^|\n)# Files(\n|$$)/,/(^|\n)# Finished Make data base/ {if ($$1 !~ "^[#.]") {print $$1}}' | sort | egrep -v -e '^[^[:alnum:]]' -e '^$@$$'

ex-clone-custom:
	cargo run --package=sleipnir-mutator --example clone_solx_custom

ex-rpc:
	cargo run --package=sleipnir-rpc --example rpc

ex-rpc-release:
	cargo run --release --package=sleipnir-rpc --example rpc

fmt:
	cargo +nightly fmt -- --config-path rustfmt-nightly.toml

.PHONY:
	list test test-log test-bank fmt ex-clone-custom ex-rpc ex-rpc-release
