DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
DEPLOY_DIR := $(DIR)target/deploy
ROOT_DEPLOY_DIR := $(DIR)../target/deploy

RUST_LOG ?= 'warn,geyser_plugin=warn,magicblock=trace,rpc=trace,bank=trace,banking_stage=warn,solana_geyser_plugin_manager=warn,solana_svm=warn,test_tools=trace,schedulecommit_test=trace,' \

FLEXI_COUNTER_DIR := $(DIR)programs/flexi-counter
SCHEDULECOMMIT_DIR := $(DIR)programs/schedulecommit
SCHEDULECOMMIT_SECURITY_DIR := $(DIR)programs/schedulecommit-security
COMMITTOR_PROGRAM_DIR := $(DIR)../magicblock-committor-program

FLEXI_COUNTER_SRC := $(shell find $(FLEXI_COUNTER_DIR) -name '*.rs' -o -name '*.toml')
SCHEDULECOMMIT_SRC := $(shell find $(SCHEDULECOMMIT_DIR) -name '*.rs' -o -name '*.toml')
SCHEDULECOMMIT_SECURITY_SRC := $(shell find $(SCHEDULECOMMIT_SECURITY_DIR) -name '*.rs' -o -name '*.toml')
COMMITTOR_PROGRAM_SRC := $(shell find $(COMMITTOR_PROGRAM_DIR) -name '*.rs' -o -name '*.toml')

FLEXI_COUNTER_SO := $(DEPLOY_DIR)/program_flexi_counter.so
SCHEDULECOMMIT_SO := $(DEPLOY_DIR)/program_schedulecommit.so
SCHEDULECOMMIT_SECURITY_SO := $(DEPLOY_DIR)/program_schedulecommit_security.so
COMMITTOR_PROGRAM_SO := $(ROOT_DEPLOY_DIR)/magicblock_committor_program.so

PROGRAMS_SO := $(FLEXI_COUNTER_SO) $(SCHEDULECOMMIT_SO) $(SCHEDULECOMMIT_SECURITY_SO) $(COMMITTOR_PROGRAM_SO)

# Validator binaries path
VALIDATOR_BINARY := $(DIR)../target/debug/magicblock-validator
VALIDATOR_BINARY_RELEASE := $(DIR)../target/release/magicblock-validator

list:
	@cat Makefile | grep "^[a-z].*:" | sed 's/:.*//g'
list-programs:
	@echo $(PROGRAMS_SO)

# Build validator binaries once
build-validator:
	cargo build --package magicblock-validator --manifest-path $(DIR)../Cargo.toml
	cargo build --package magicblock-validator --release --manifest-path $(DIR)../Cargo.toml

test: build-validator $(PROGRAMS_SO)
	RUST_BACKTRACE=1 \
	RUST_LOG=$(RUST_LOG) \
	VALIDATOR_BINARY=$(VALIDATOR_BINARY) \
	VALIDATOR_BINARY_RELEASE=$(VALIDATOR_BINARY_RELEASE) \
	cargo run --package test-runner --bin run-tests

test-force-mb: build-validator $(PROGRAMS_SO) test-ledger-restore
	RUST_LOG=$(RUST_LOG) \
	FORCE_MAGIC_BLOCK_VALIDATOR=1 \
	VALIDATOR_BINARY=$(VALIDATOR_BINARY) \
	VALIDATOR_BINARY_RELEASE=$(VALIDATOR_BINARY_RELEASE) \
	cargo run --package test-runner --bin run-tests

test-schedulecommit:
	RUN_TESTS=schedulecommit \
	$(MAKE) test

test-issues-frequent-commits:
	RUN_TESTS=issues_frequent_commmits \
	$(MAKE) test

test-cloning:
	RUN_TESTS=cloning \
	$(MAKE) test

test-restore-ledger:
	RUN_TESTS=restore_ledger \
	$(MAKE) test

test-magicblock-api:
	RUN_TESTS=magicblock_api \
	$(MAKE) test

test-table-mania:
	RUN_TESTS=table_mania \
	$(MAKE) test

test-committor:
	RUN_TESTS=committor \
	$(MAKE) test

test-pubsub:
	RUN_TESTS=pubsub \
	$(MAKE) test

test-config:
	RUN_TESTS=config \
	$(MAKE) test

$(FLEXI_COUNTER_SO): $(FLEXI_COUNTER_SRC)
	cargo build-sbf --manifest-path $(FLEXI_COUNTER_DIR)/Cargo.toml
$(SCHEDULECOMMIT_SO): $(SCHEDULECOMMIT_SRC)
	cargo build-sbf --manifest-path $(SCHEDULECOMMIT_DIR)/Cargo.toml
$(SCHEDULECOMMIT_SECURITY_SO): $(SCHEDULECOMMIT_SECURITY_SRC)
	cargo build-sbf --manifest-path $(SCHEDULECOMMIT_SECURITY_DIR)/Cargo.toml
$(COMMITTOR_PROGRAM_SO): $(COMMITTOR_PROGRAM_SRC)
	cargo build-sbf --manifest-path $(COMMITTOR_PROGRAM_DIR)/Cargo.toml

deploy-flexi-counter: $(FLEXI_COUNTER_SO)
	solana program deploy \
		-u localhost \
		--keypair $(DIR)/target/deploy/program_flexi_counter-keypair.json \
		--program-id $(DIR)/programs/flexi-counter/keys/f1exzKGtdeVX3d6UXZ89cY7twiNJe9S5uq84RTA4Rq4.json \
		$(DIR)/target/deploy/program_flexi_counter.so


.PHONY: test test-force-mb test-schedulecommit test-issues-frequent-commits test-cloning test-restore-ledger test-magicblock-api test-table-mania test-committor test-pubsub test-config deploy-flexi-counter list build-validator
