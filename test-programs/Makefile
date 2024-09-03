DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

test:
	RUST_LOG='warn,geyser_plugin=trace,sleipnir=trace,rpc=trace,bank=trace,banking_stage=trace,solana_geyser_plugin_manager=debug,solana_svm=debug,test_tools=trace,schedulecommit_test=trace,' \
	cargo run --bin run-schedule-commit-tests
