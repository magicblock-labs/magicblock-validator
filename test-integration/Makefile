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

list:
	@cat Makefile | grep "^[a-z].*:" | sed 's/:.*//g'
list-programs:
	@echo $(PROGRAMS_SO)

test: $(PROGRAMS_SO)
	RUST_BACKTRACE=1 \
	RUST_LOG=$(RUST_LOG) \
	cargo run --package test-runner --bin run-tests

test-force-mb: $(PROGRAMS_SO) test-ledger-restore
	RUST_LOG=$(RUST_LOG) \
	FORCE_MAGIC_BLOCK_VALIDATOR=1 \
	cargo run --package test-runner --bin run-tests

test-schedulecommit:
	RUN_TESTS=schedulecommit \
	$(MAKE) test
setup-schedulecommit-devnet:
	RUN_TESTS=schedulecommit \
	SETUP_ONLY=devnet \
	$(MAKE) test
setup-schedulecommit-both:
	RUN_TESTS=schedulecommit \
	SETUP_ONLY=both \
	$(MAKE) test

test-issues-frequent-commits:
	RUN_TESTS=issues_frequent_commits \
	$(MAKE) test
setup-issues-frequent-commits-devnet:
	RUN_TESTS=issues_frequent_commits \
	SETUP_ONLY=devnet \
	$(MAKE) test
setup-issues-frequent-commits-both:
	RUN_TESTS=issues_frequent_commits \
	SETUP_ONLY=both \
	$(MAKE) test

test-cloning:
	RUN_TESTS=cloning \
	$(MAKE) test
setup-cloning-devnet:
	RUN_TESTS=cloning \
	SETUP_ONLY=devnet \
	$(MAKE) test
setup-cloning-both:
	RUN_TESTS=cloning \
	SETUP_ONLY=both \
	$(MAKE) test

test-restore-ledger:
	RUN_TESTS=restore_ledger \
	$(MAKE) test
setup-restore-ledger-devnet:
	RUN_TESTS=restore_ledger \
	SETUP_ONLY=devnet \
	$(MAKE) test

test-magicblock-api:
	RUN_TESTS=magicblock_api \
	$(MAKE) test
setup-magicblock-api-devnet:
	RUN_TESTS=magicblock_api \
	SETUP_ONLY=devnet \
	$(MAKE) test
setup-magicblock-api-both:
	RUN_TESTS=magicblock_api \
	SETUP_ONLY=both \
	$(MAKE) test

test-table-mania:
	RUN_TESTS=table_mania \
	$(MAKE) test
setup-table-mania-devnet:
	RUN_TESTS=table_mania \
	SETUP_ONLY=devnet \
	$(MAKE) test

test-committor:
	RUN_TESTS=committor \
	$(MAKE) test
setup-committor-devnet:
	RUN_TESTS=committor \
	SETUP_ONLY=devnet \
	$(MAKE) test

test-pubsub:
	RUN_TESTS=pubsub \
	$(MAKE) test
setup-pubsub-ephem:
	RUN_TESTS=pubsub \
	SETUP_ONLY=ephem \
	$(MAKE) test

test-config:
	RUN_TESTS=config \
	$(MAKE) test
setup-config-devnet:
	RUN_TESTS=config \
	SETUP_ONLY=devnet \
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


.PHONY: test test-force-mb test-schedulecommit test-issues-frequent-commits test-cloning test-restore-ledger test-magicblock-api test-table-mania test-committor test-pubsub test-config deploy-flexi-counter list
