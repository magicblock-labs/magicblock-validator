DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

fmt:
	cargo +nightly fmt -- --config-path $(DIR)rustfmt-nightly.toml

list:
	@LC_ALL=C $(MAKE) -pRrq -f $(firstword $(MAKEFILE_LIST)) : 2>/dev/null | awk -v RS= -F: '/(^|\n)# Files(\n|$$)/,/(^|\n)# Finished Make data base/ {if ($$1 !~ "^[#.]") {print $$1}}' | sort | egrep -v -e '^[^[:alnum:]]' -e '^$@$$'

manual-test-laser:
	@echo "Checking for Helius API key"
	@if [ -z "$$HELIUS_API_KEY" ]; then \
		echo "Error: HELIUS_API_KEY environment variable not set"; \
		exit 1; \
	fi
	@echo
	@echo "## Extracting keypair path..."
	$(eval KEYPAIR_PATH := $(shell solana config get | grep "Keypair Path:" | cut -d: -f2 | xargs))
	@echo "Keypair path: $(KEYPAIR_PATH)"
	@echo
	@echo "## Airdropping 1 SOL to pubkey on devnet"
	$(eval PUBKEY := $(shell solana-keygen pubkey $(KEYPAIR_PATH)))
	@echo "Pubkey: $(PUBKEY)"
	@echo "Airdropping 1 SOL to $(PUBKEY)..."
	@solana airdrop 1 $(PUBKEY) --url devnet || true
	@echo
	@echo "## Creating validator config..."
	@echo "[accounts]" > /tmp/mb-test-laser.toml
	@echo "remote.url = \"https://devnet.helius-rpc.com/?api-key=$(HELIUS_API_KEY)\"" >> /tmp/mb-test-laser.toml
	@echo "remote.ws-url = [\"https://laserstream-devnet-ewr.helius-rpc.com?api-key=$(HELIUS_API_KEY)\"]" >> /tmp/mb-test-laser.toml
	@echo "" >> /tmp/mb-test-laser.toml
	@echo "[rpc]" >> /tmp/mb-test-laser.toml
	@echo "addr = \"0.0.0.0\"" >> /tmp/mb-test-laser.toml
	@echo "port = 9988" >> /tmp/mb-test-laser.toml
	@echo
	@echo "## Starting validator in background..."
	RUST_LOG=warn,magicblock=info,magicblock_chainlink=debug,magicblock_accounts=debug \
			cargo run --bin magicblock-validator --manifest-path=$(DIR)../Cargo.toml \
			-- --ledger-resume-strategy-kind=reset \
			-- /tmp/mb-test-laser.toml \
			> /tmp/validator.log 2>&1 &
	@echo $$! > /tmp/validator.pid
	@echo
	@echo "### Waiting until validator listens on port 9988... via solana cluster-version"
	@until solana cluster-version --url http://127.0.0.1:9988 >/dev/null 2>&1; do \
		sleep 1; \
	done
	@echo
	@echo "## Running laser test..."
	KEYPAIR_PATH=$(KEYPAIR_PATH) \
		RUST_LOG=info \
		cargo run  --bin helius-laser
	@echo
	@if [ $$? -ne 0 ]; then \
		echo "## Laser test failed, showing validator logs"; \
		cat /tmp/validator.log || true; \
		echo "⛔ TEST FAILED"; \
	else \
		echo "✅ TEST PASSED"; \
	fi
	@echo
	@if [ -f /tmp/validator.pid ]; then \
		echo "## Shutting down validator"; \
		kill $$(cat /tmp/validator.pid) 2>/dev/null || true; \
		rm /tmp/validator.pid /tmp/validator.log ; \
	fi

.PHONY: fmt list manual-test-laser
