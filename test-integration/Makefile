DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

test: test-ledger-restore
	RUST_LOG='warn,geyser_plugin=trace,sleipnir=trace,rpc=trace,bank=trace,banking_stage=trace,solana_geyser_plugin_manager=debug,solana_svm=debug,test_tools=trace,schedulecommit_test=trace,' \
	RUST_BACKTRACE=1 \
	cargo run --package test-runner --bin run-tests

test-force-mb: test-ledger-restore
	RUST_LOG='warn,geyser_plugin=trace,sleipnir=trace,rpc=trace,bank=trace,banking_stage=trace,solana_geyser_plugin_manager=debug,solana_svm=debug,test_tools=trace,schedulecommit_test=trace,' \
	FORCE_MAGIC_BLOCK_VALIDATOR=1 \
	cargo run --package test-runner --bin run-tests

test-ledger-restore: build-flexi-counter
	cargo test --package test-ledger-restore -- --test-threads=1 --nocapture

test-ledger-restore-release: build-flexi-counter
	RELEASE=1 cargo test --package test-ledger-restore -- --test-threads=1 --nocapture

build-flexi-counter:
	cargo build-sbf --manifest-path $(DIR)/programs/flexi-counter/Cargo.toml

deploy-flexi-counter: build-flexi-counter
	solana program deploy \
		-u localhost \
		--keypair $(DIR)/target/deploy/program_flexi_counter-keypair.json \
		--program-id $(DIR)/programs/flexi-counter/keys/f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4.json \
		$(DIR)/target/deploy/program_flexi_counter.so


.PHONY: test, test-force-mb, test-ledger-restore, build-flexi-counter, deploy-flexi-counter
