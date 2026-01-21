DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

GRPC_ACCOUNT_TRACKING_DIR := $(DIR)grpc-account-tracking
GRPC_ACCOUNT_TRACKING_SH_DIR := $(GRPC_ACCOUNT_TRACKING_DIR)/sh

fmt:
	cargo +nightly fmt -- --config-path $(DIR)rustfmt-nightly.toml

list:
	@LC_ALL=C $(MAKE) -pRrq -f $(firstword $(MAKEFILE_LIST)) : 2>/dev/null | awk -v RS= -F: '/(^|\n)# Files(\n|$$)/,/(^|\n)# Finished Make data base/ {if ($$1 !~ "^[#.]") {print $$1}}' | sort | egrep -v -e '^[^[:alnum:]]' -e '^$@$$'

test-grpc-account-tracking:
	@if [ -z "$$HELIUS_API_KEY" ]; then \
		echo "Error: HELIUS_API_KEY environment variable and optionally TRITON_API_KEY need to be set."; \
		exit 1; \
	fi
	$(eval KEYPAIR_PATH := $(shell $(GRPC_ACCOUNT_TRACKING_SH_DIR)/01_extract_keypair_path.sh))
	$(GRPC_ACCOUNT_TRACKING_SH_DIR)/02_airdrop.sh $(KEYPAIR_PATH) || true
	$(GRPC_ACCOUNT_TRACKING_SH_DIR)/03_create-config.sh
	@echo
	@echo "## Starting validator in background..."
	RUST_LOG=warn,magicblock=info,magicblock_chainlink=debug,magicblock_accounts=debug \
			cargo run --bin magicblock-validator --manifest-path=$(DIR)../Cargo.toml \
			-- /tmp/mb-test-grpc-account-tracking.toml \
			> /tmp/validator.log 2>&1 &
	@echo $$! > /tmp/validator.pid
	@echo
	@echo "### Waiting until validator listens on port 9988... via solana cluster-version"
	@until solana cluster-version --url http://127.0.0.1:9988 >/dev/null 2>&1; do \
		sleep 1; \
	done
	@echo "## Running account tracking test..."
	@KEYPAIR_PATH=$(KEYPAIR_PATH) RUST_LOG=info \
		cargo run --bin grpc-account-tracking || { \
			echo "## Account tracking test failed, showing validator logs"; \
			cat /tmp/validator.log || true; \
			echo "⛔ TEST FAILED"; \
		}
	@echo "✅ TEST COMPLETED"
	@echo
	@if [ -f /tmp/validator.pid ]; then \
		echo "## Shutting down validator"; \
		kill $$(cat /tmp/validator.pid) 2>/dev/null || true; \
		rm /tmp/validator.pid /tmp/validator.log /tmp/mb-test-grpc-account-tracking.toml ; \
	fi

.PHONY: fmt list test-grpc-account-tracking