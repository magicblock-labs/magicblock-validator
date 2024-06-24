MAKEFILE_PATH := $(abspath $(lastword $(MAKEFILE_LIST)))
MAKEFILE_DIR := $(dir $(MAKEFILE_PATH))

AUTHORITY_KEY := CTVAe8vsyXkBKDPcJRUYMEoEm7MiZ9wF3iPENBHFtBEH
AUTHORITY_KEYPAIR_FILE := $(MAKEFILE_DIR)/keys/$(AUTHORITY_KEY).json
TC_PROGRAM_KEYPAIR_FILE := $(MAKEFILE_DIR)/elfs/triggercommit_program-keypair.json

solana-deploy-local:
	cd $(MAKEFILE_DIR)
	solana airdrop 5 -u localhost $(AUTHORITY_KEYPAIR_FILE)
	solana program deploy -u localhost \
		--keypair $(AUTHORITY_KEYPAIR_FILE) \
		--program-id $(TC_PROGRAM_KEYPAIR_FILE) \
		$(MAKEFILE_DIR)/elfs/triggercommit_program.so
