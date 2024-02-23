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

build:
	. $(DIR)/sh/source/macos/build-exports-ci && \
	RINF=1 cargo build
