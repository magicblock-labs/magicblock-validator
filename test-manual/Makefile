DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

HELIUS_LASER_DIR := $(DIR)helius-laser
HELIUS_LASER_SH_DIR := $(HELIUS_LASER_DIR)/sh

fmt:
	cargo +nightly fmt -- --config-path $(DIR)rustfmt-nightly.toml

list:
	@LC_ALL=C $(MAKE) -pRrq -f $(firstword $(MAKEFILE_LIST)) : 2>/dev/null | awk -v RS= -F: '/(^|\n)# Files(\n|$$)/,/(^|\n)# Finished Make data base/ {if ($$1 !~ "^[#.]") {print $$1}}' | sort | egrep -v -e '^[^[:alnum:]]' -e '^$@$$'

test-laser:
	@if [ -z "$$HELIUS_API_KEY" ] && [ -z "$$TRITON_API_KEY" ]; then \
		echo "Error: HELIUS_API_KEY or TRITON_API_KEY environment variable not set"; \
		exit 1; \
	fi
	$(eval KEYPAIR_PATH := $(shell $(HELIUS_LASER_SH_DIR)/01_extract_keypair_path.sh))
	$(HELIUS_LASER_SH_DIR)/02_airdrop.sh $(KEYPAIR_PATH) || true
	$(HELIUS_LASER_SH_DIR)/03_create-config.sh
	@echo
	@echo "## Starting validator in background..."
	RUST_LOG=warn,magicblock=info,magicblock_chainlink=debug,magicblock_accounts=debug \
			cargo run --bin magicblock-validator --manifest-path=$(DIR)../Cargo.toml \
			-- /tmp/mb-test-laser.toml \
			> /tmp/validator.log 2>&1 &
	@echo $$! > /tmp/validator.pid
	@echo
	@echo "### Waiting until validator listens on port 9988... via solana cluster-version"
	@until solana cluster-version --url http://127.0.0.1:9988 >/dev/null 2>&1; do \
		sleep 1; \
	done
	@echo "## Running laser test..."
	@KEYPAIR_PATH=$(KEYPAIR_PATH) RUST_LOG=info \
		cargo run --bin helius-laser || { \
			echo "## Laser test failed, showing validator logs"; \
			cat /tmp/validator.log || true; \
			echo "⛔ TEST FAILED"; \
		}
	@echo "✅ TEST PASSED"
	@echo
	@if [ -f /tmp/validator.pid ]; then \
		echo "## Shutting down validator"; \
		kill $$(cat /tmp/validator.pid) 2>/dev/null || true; \
		rm /tmp/validator.pid /tmp/validator.log /tmp/mb-test-laser.toml ; \
	fi

.PHONY: fmt list test-laser
