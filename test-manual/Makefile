DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

fmt:
	cargo +nightly fmt -- --config-path $(DIR)rustfmt-nightly.toml

list:
	@LC_ALL=C $(MAKE) -pRrq -f $(firstword $(MAKEFILE_LIST)) : 2>/dev/null | awk -v RS= -F: '/(^|\n)# Files(\n|$$)/,/(^|\n)# Finished Make data base/ {if ($$1 !~ "^[#.]") {print $$1}}' | sort | egrep -v -e '^[^[:alnum:]]' -e '^$@$$'

manual-test-laser:
	@echo "Preparing for laser test..."
	@if [ -z "$$HELIUS_API_KEY" ]; then \
		echo "Error: HELIUS_API_KEY environment variable not set"; \
		exit 1; \
	fi
	@echo "Extracting keypair path..."
	$(eval KEYPAIR_PATH := $(shell solana config get | grep "Keypair Path:" | cut -d: -f2 | xargs))
	@echo "Keypair path: $(KEYPAIR_PATH)"
	@echo "Extracting pubkey..."
	$(eval PUBKEY := $(shell solana-keygen pubkey $(KEYPAIR_PATH)))
	@echo "Pubkey: $(PUBKEY)"
	@echo "Airdropping 1 SOL to $(PUBKEY)..."
	@solana airdrop 1 $(PUBKEY) --url devnet || true
	@echo "Creating validator config..."
	@echo "[accounts]" > /tmp/mb-test-laser.toml
	@echo "remote.url = \"https://devnet.helius-rpc.com/?api-key=$(HELIUS_API_KEY)\"" >> /tmp/mb-test-laser.toml
	@echo "remote.ws-url = [\"https://laserstream-devnet-ewr.helius-rpc.com?api-key=$(HELIUS_API_KEY)\"]" >> /tmp/mb-test-laser.toml
	@echo "" >> /tmp/mb-test-laser.toml
	@echo "[rpc]" >> /tmp/mb-test-laser.toml
	@echo "addr = \"0.0.0.0\"" >> /tmp/mb-test-laser.toml
	@echo "port = 8899" >> /tmp/mb-test-laser.toml
	@echo "Starting validator in background..."
	RUST_LOG=magicblock_chainlink=debug,magicblock_accounts=debug \
			 cargo run --bin magicblock-validator -- \
			 --config /tmp/mb-test-laser.toml --ephemeral \
			 > /tmp/validator.log 2>&1 &
	@echo $$! > /tmp/validator.pid
	@sleep 3
	@echo "Running laser test..."
	cd helius-laser && KEYPAIR_PATH=$(KEYPAIR_PATH) RUST_LOG=info cargo run
	@echo "Validator logs:"
	@cat /tmp/validator.log || true
	@echo "Cleaning up validator..."
	@if [ -f /tmp/validator.pid ]; then kill $$(cat /tmp/validator.pid) 2>/dev/null || true; rm /tmp/validator.pid /tmp/validator.log; fi
