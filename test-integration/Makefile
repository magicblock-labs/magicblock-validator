# Makefile for building and testing Solana programs and test suites
DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
DEPLOY_DIR := $(DIR)target/deploy
ROOT_DEPLOY_DIR := $(DIR)../target/deploy

RUST_LOG ?= 'info'

SCHEDULECOMMIT_DIR := $(DIR)programs/schedulecommit
SCHEDULECOMMIT_SECURITY_DIR := $(DIR)programs/schedulecommit-security
COMMITTOR_PROGRAM_DIR := $(DIR)../magicblock-committor-program

SCHEDULECOMMIT_SRC := $(shell find $(SCHEDULECOMMIT_DIR) -name '*.rs' -o -name '*.toml')
SCHEDULECOMMIT_SECURITY_SRC := $(shell find $(SCHEDULECOMMIT_SECURITY_DIR) -name '*.rs' -o -name '*.toml')
COMMITTOR_PROGRAM_SRC := $(shell find $(COMMITTOR_PROGRAM_DIR) -name '*.rs' -o -name '*.toml')

SCHEDULECOMMIT_SO := $(DEPLOY_DIR)/program_schedulecommit.so
SCHEDULECOMMIT_SECURITY_SO := $(DEPLOY_DIR)/program_schedulecommit_security.so
COMMITTOR_PROGRAM_SO := $(ROOT_DEPLOY_DIR)/magicblock_committor_program.so

PROGRAMS_SO := $(SCHEDULECOMMIT_SO) $(SCHEDULECOMMIT_SECURITY_SO) $(COMMITTOR_PROGRAM_SO)
COMMITTOR_IX_SINGLE_TESTS := committor,committor_single_large
COMMITTOR_BUNDLE_TESTS := committor_bundles,committor_bundles_undelegate,committor_bundles_8
COMMITTOR_BUNDLE_HEAVY_TESTS := committor_bundles_heavy,committor_bundles_heavy_tail
COMMITTOR_INTENT_EXECUTOR_TESTS := committor_intent_executor,committor_intent_executor_recovery_basic,committor_intent_executor_recovery,committor_intent_executor_callbacks
COMMITTOR_TESTS := $(COMMITTOR_IX_SINGLE_TESTS),committor_preparators,committor_ix_order,committor_ix_multi,$(COMMITTOR_BUNDLE_TESTS),committor_intent_bundles,$(COMMITTOR_BUNDLE_HEAVY_TESTS),committor_commitfinalize,$(COMMITTOR_INTENT_EXECUTOR_TESTS)

list:
	@cat Makefile | grep "^[a-z].*:" | sed 's/:.*//g'
list-programs:
	@echo $(PROGRAMS_SO)

programs: $(PROGRAMS_SO)

ci-test: test

test: $(PROGRAMS_SO)
	if [ -n "$(INTEGRATION_TEST_RUNNER_BIN)" ] && [ -x "$(INTEGRATION_TEST_RUNNER_BIN)" ]; then \
		RUST_BACKTRACE=1 \
		RUST_LOG=$(RUST_LOG) \
		CARGO_MANIFEST_DIR="$(DIR)test-runner" \
		"$(INTEGRATION_TEST_RUNNER_BIN)"; \
	else \
		RUST_BACKTRACE=1 \
		RUST_LOG=$(RUST_LOG) \
		cargo run --package test-runner --bin run-tests; \
	fi

test-force-mb: $(PROGRAMS_SO)
	RUST_LOG=$(RUST_LOG) \
	FORCE_MAGIC_BLOCK_VALIDATOR=1 \
	cargo run --package test-runner --bin run-tests

test-aml:
	RUN_TESTS=aml \
	$(MAKE) test
setup-aml-devnet:
	RUST_LOG_STYLE=none \
	RUN_TESTS=aml \
	SETUP_ONLY=devnet \
	$(MAKE) test

test-schedulecommit:
	RUN_TESTS=schedulecommit \
	$(MAKE) test
setup-schedulecommit-devnet:
	RUST_LOG_STYLE=none \
	RUN_TESTS=schedulecommit \
	SETUP_ONLY=devnet \
	$(MAKE) test
setup-schedulecommit-ephem:
	RUST_LOG_STYLE=none \
	RUN_TESTS=schedulecommit \
	SETUP_ONLY=ephem \
	$(MAKE) test
setup-schedulecommit-both:
	RUST_LOG_STYLE=none \
	RUN_TESTS=schedulecommit \
	SETUP_ONLY=both \
	$(MAKE) test

test-cloning:
	RUN_TESTS=cloning \
	$(MAKE) test
setup-cloning-devnet:
	RUST_LOG_STYLE=none \
	RUN_TESTS=cloning \
	SETUP_ONLY=devnet \
	$(MAKE) test
setup-cloning-ephem:
	RUST_LOG_STYLE=none \
	RUN_TESTS=cloning \
	SETUP_ONLY=ephem \
	$(MAKE) test
setup-cloning-both:
	RUST_LOG_STYLE=none \
	RUN_TESTS=cloning \
	SETUP_ONLY=both \
	$(MAKE) test


test-validator-services:
	RUN_TESTS=validator_services \
	$(MAKE) test
setup-validator-services-devnet:
	RUST_LOG_STYLE=none \
	RUN_TESTS=validator_services \
	SETUP_ONLY=devnet \
	$(MAKE) test
setup-validator-services-ephem:
	RUST_LOG_STYLE=none \
	RUN_TESTS=validator_services \
	SETUP_ONLY=ephem \
	$(MAKE) test
setup-validator-services-both:
	RUST_LOG_STYLE=none \
	RUN_TESTS=validator_services \
	SETUP_ONLY=both \
	$(MAKE) test

test-table-mania:
	RUN_TESTS=table_mania \
	$(MAKE) test
setup-table-mania-devnet:
	RUST_LOG_STYLE=none \
	RUN_TESTS=table_mania \
	SETUP_ONLY=devnet \
	$(MAKE) test

test-committor:
	RUN_TESTS=$(COMMITTOR_TESTS) \
	$(MAKE) test
test-committor-ix-singles:
	RUN_TESTS=$(COMMITTOR_IX_SINGLE_TESTS) \
	$(MAKE) test
test-committor-preparators:
	RUN_TESTS=committor_preparators \
	$(MAKE) test
test-committor-ix-order:
	RUN_TESTS=committor_ix_order \
	$(MAKE) test
test-committor-ix-multi:
	RUN_TESTS=committor_ix_multi \
	$(MAKE) test
test-committor-bundles:
	RUN_TESTS=$(COMMITTOR_BUNDLE_TESTS) \
	$(MAKE) test
test-committor-intent-bundles:
	RUN_TESTS=committor_intent_bundles \
	$(MAKE) test
test-committor-bundles-heavy:
	RUN_TESTS=$(COMMITTOR_BUNDLE_HEAVY_TESTS) \
	$(MAKE) test
test-committor-commitfinalize:
	RUN_TESTS=committor_commitfinalize \
	$(MAKE) test
test-committor-intent-executor:
	RUN_TESTS=$(COMMITTOR_INTENT_EXECUTOR_TESTS) \
	$(MAKE) test
test-committor-intent-executor-errors:
	RUN_TESTS=committor_intent_executor \
	$(MAKE) test
test-committor-intent-executor-recovery:
	RUN_TESTS=committor_intent_executor_recovery_basic,committor_intent_executor_recovery,committor_intent_executor_callbacks \
	$(MAKE) test
setup-committor-devnet:
	RUST_LOG_STYLE=none \
	RUN_TESTS=$(COMMITTOR_TESTS) \
	SETUP_ONLY=devnet \
	$(MAKE) test

test-config:
	RUN_TESTS=config \
	$(MAKE) test
setup-config-devnet:
	RUST_LOG_STYLE=none \
	RUN_TESTS=config \
	SETUP_ONLY=devnet \
	$(MAKE) test

test-schedule-intents:
	RUN_TESTS=schedule_intents \
	$(MAKE) test
setup-schedule-intents-devnet:
	RUST_LOG_STYLE=none \
	RUN_TESTS=schedule_intents \
	SETUP_ONLY=devnet \
	$(MAKE) test
setup-schedule-intents-ephem:
	RUST_LOG_STYLE=none \
	RUN_TESTS=schedule_intents \
	SETUP_ONLY=ephem \
	$(MAKE) test
setup-schedule-intents-both:
	RUST_LOG_STYLE=none \
	RUN_TESTS=schedule_intents \
	SETUP_ONLY=both \
	$(MAKE) test

test-task-scheduler:
	RUN_TESTS=task-scheduler \
	$(MAKE) test
setup-task-scheduler-devnet:
	RUN_TESTS=task-scheduler \
	SETUP_ONLY=devnet \
	$(MAKE) test

$(SCHEDULECOMMIT_SO): $(SCHEDULECOMMIT_SRC)
	cargo build-sbf --manifest-path $(SCHEDULECOMMIT_DIR)/Cargo.toml \
		--sbf-out-dir $(DEPLOY_DIR) --quiet
$(SCHEDULECOMMIT_SECURITY_SO): $(SCHEDULECOMMIT_SECURITY_SRC)
	cargo build-sbf --manifest-path $(SCHEDULECOMMIT_SECURITY_DIR)/Cargo.toml \
		--sbf-out-dir $(DEPLOY_DIR) --quiet
$(COMMITTOR_PROGRAM_SO): $(COMMITTOR_PROGRAM_SRC)
	cargo build-sbf --manifest-path $(COMMITTOR_PROGRAM_DIR)/Cargo.toml \
	--sbf-out-dir $(ROOT_DEPLOY_DIR)/ --quiet

fmt:
	cargo +nightly fmt -- --config-path ../rustfmt-nightly.toml

lint:
	cargo clippy --all-targets -- -D warnings

ci-fmt:
	cargo +nightly fmt --check -- --config-path ../rustfmt-nightly.toml

ci-lint:
	cargo clippy --workspace --all-targets --locked -- -D warnings

.PHONY: \
	ci-test \
	ci-fmt \
	lint \
	ci-lint \
	fmt \
	list \
	list-programs \
	programs \
	setup-aml-devnet \
	setup-cloning-both \
	setup-cloning-devnet \
	setup-cloning-ephem \
	setup-committor-devnet \
	setup-config-devnet \
	setup-validator-services-both \
	setup-validator-services-devnet \
	setup-validator-services-ephem \
	setup-schedule-intents-both \
	setup-schedule-intents-devnet \
	setup-schedule-intents-ephem \
	setup-schedulecommit-both \
	setup-schedulecommit-devnet \
	setup-schedulecommit-ephem \
	setup-table-mania-devnet \
	setup-task-scheduler-devnet \
	test \
	test-aml \
	test-cloning \
	test-committor \
	test-committor-ix-singles \
	test-committor-preparators \
	test-committor-ix-order \
	test-committor-ix-multi \
	test-committor-bundles \
	test-committor-intent-bundles \
	test-committor-bundles-heavy \
	test-committor-commitfinalize \
	test-committor-intent-executor \
	test-config \
	test-force-mb \
	test-schedule-intents \
	test-schedulecommit \
	test-table-mania \
	test-task-scheduler \
	test-validator-services
