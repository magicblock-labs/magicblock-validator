DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

DEPLOY_DIR := $(DIR)target/deploy

FLEXI_COUNTER_DIR := $(DIR)programs/flexi-counter
SCHEDULECOMMIT_DIR := $(DIR)programs/schedulecommit
SCHEDULECOMMIT_SECURITY_DIR := $(DIR)programs/schedulecommit-security

FLEXI_COUNTER_SRC := $(shell find $(FLEXI_COUNTER_DIR) -name '*.rs' -o -name '*.toml')
SCHEDULECOMMIT_SRC := $(shell find $(SCHEDULECOMMIT_DIR) -name '*.rs' -o -name '*.toml')
SCHEDULECOMMIT_SECURITY_SRC := $(shell find $(SCHEDULECOMMIT_SECURITY_DIR) -name '*.rs' -o -name '*.toml')

FLEXI_COUNTER_SO := $(DEPLOY_DIR)/program_flexi_counter.so
SCHEDULECOMMIT_SO := $(DEPLOY_DIR)/program_schedulecommit.so
SCHEDULECOMMIT_SECURITY_SO := $(DEPLOY_DIR)/program_schedulecommit_security.so

PROGRAMS_SO := $(FLEXI_COUNTER_SO) $(SCHEDULECOMMIT_SO) $(SCHEDULECOMMIT_SECURITY_SO)

list-tasks:
	@cat Makefile | grep "^[a-z].*:" | sed 's/:.*//g'
list-programs:
	@echo $(PROGRAMS_SO)

test: $(PROGRAMS_SO) test-ledger-restore
	RUST_LOG='warn,geyser_plugin=trace,sleipnir=trace,rpc=trace,bank=trace,banking_stage=trace,solana_geyser_plugin_manager=debug,solana_svm=debug,test_tools=trace,schedulecommit_test=trace,' \
	RUST_BACKTRACE=1 \
	cargo run --package test-runner --bin run-tests

test-force-mb: $(PROGRAMS_SO) test-ledger-restore
	RUST_LOG='warn,geyser_plugin=trace,sleipnir=trace,rpc=trace,bank=trace,banking_stage=trace,solana_geyser_plugin_manager=debug,solana_svm=debug,test_tools=trace,schedulecommit_test=trace,' \
	FORCE_MAGIC_BLOCK_VALIDATOR=1 \
	cargo run --package test-runner --bin run-tests

test-ledger-restore: $(FLEXI_COUNTER_SO)
	cargo test --package test-ledger-restore -- --test-threads=1 --nocapture

test-ledger-restore-release: $(FLEXI_COUNTER_SO)
	RELEASE=1 cargo test --package test-ledger-restore -- --test-threads=1 --nocapture

$(FLEXI_COUNTER_SO): $(FLEXI_COUNTER_SRC)
	cargo build-sbf --manifest-path $(FLEXI_COUNTER_DIR)/Cargo.toml
$(SCHEDULECOMMIT_SO): $(SCHEDULECOMMIT_SRC)
	cargo build-sbf --manifest-path $(SCHEDULECOMMIT_DIR)/Cargo.toml
$(SCHEDULECOMMIT_SECURITY_SO): $(SCHEDULECOMMIT_SECURITY_SRC)
	cargo build-sbf --manifest-path $(SCHEDULECOMMIT_SECURITY_DIR)/Cargo.toml

deploy-flexi-counter: build-flexi-counter
	solana program deploy \
		-u localhost \
		--keypair $(DIR)/target/deploy/program_flexi_counter-keypair.json \
		--program-id $(DIR)/programs/flexi-counter/keys/f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4.json \
		$(DIR)/target/deploy/program_flexi_counter.so


.PHONY: test test-force-mb test-ledger-restore test-ledger-restore-release deploy-flexi-counter
