DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

test:
	RUST_LOG='warn,geyser_plugin=trace,sleipnir=trace,rpc=trace,bank=trace,banking_stage=trace,solana_geyser_plugin_manager=debug,solana_svm=debug,test_tools=trace,schedulecommit_test=trace,' \
	RUST_BACKTRACE=1 \
	cargo run --package test-runner --bin run-tests

test-force-mb:
	RUST_LOG='warn,geyser_plugin=trace,sleipnir=trace,rpc=trace,bank=trace,banking_stage=trace,solana_geyser_plugin_manager=debug,solana_svm=debug,test_tools=trace,schedulecommit_test=trace,' \
	FORCE_MAGIC_BLOCK_VALIDATOR=1 \
	cargo run --package test-runner --bin run-tests

