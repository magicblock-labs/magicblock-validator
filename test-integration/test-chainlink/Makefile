TEST_CHAINLINK_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))
TEST_CHAINLINK_WS_ROOT := $(TEST_CHAINLINK_DIR)../
TEST_CHAINLINK_DEPLOY_DIR := $(TEST_CHAINLINK_WS_ROOT)target/deploy
TEST_CHAINLINK_MINI_PROGRAM_DIR := $(TEST_CHAINLINK_WS_ROOT)programs/mini/

CHAINLINK_MEMOV1=Memo1UhkJRfHyvLMcVucJwxXeuD728EqVDDwQDxFMNo
CHAINLINK_MEMOV2=MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr
CHAINLINK_OTHERV1=BL5oAaURQwAVVHcgrucxJe3H5K57kCQ5Q8ys7dctqfV8
CHAINLINK_MINIV2=MiniV21111111111111111111111111111111111111
CHAINLINK_MINIV3=MiniV31111111111111111111111111111111111111

chainlink-list:
	@LC_ALL=C $(MAKE) -pRrq -f $(firstword $(MAKEFILE_LIST)) : 2>/dev/null | awk -v RS= -F: '/(^|\n)# Files(\n|$$)/,/(^|\n)# Finished Make data base/ {if ($$1 !~ "^[#.]") {print $$1}}' | sort | egrep -v -e '^[^[:alnum:]]' -e '^$@$$'

chainlink-dirs:
	@echo "TEST_CHAINLINK_DIR: $(TEST_CHAINLINK_DIR)"
	@echo "TEST_CHAINLINK_DEPLOY_DIR: $(TEST_CHAINLINK_DEPLOY_DIR)"

chainlink-prep-programs:
	$(MAKE) chainlink-build-mini-v2
	$(MAKE) chainlink-build-mini-v3

# chainlink-prep-memo-v1, chainlink-prep-memo-v2 and chainlink-prep-other-v1
# only need to run in order to refresh the account data and then copy it to
# ../configs/accounts/
chainlink-prep-memo-v1:
	mkdir -p $(TEST_CHAINLINK_DEPLOY_DIR) && \
	solana account $(CHAINLINK_MEMOV1) --output json > $(TEST_CHAINLINK_DEPLOY_DIR)/memo_v1.json

chainlink-prep-memo-v2:
	mkdir -p $(TEST_CHAINLINK_DEPLOY_DIR) && \
	solana account $(CHAINLINK_MEMOV2) --output json > $(TEST_CHAINLINK_DEPLOY_DIR)/memo_v2.json

chainlink-prep-other-v1:
	mkdir -p $(TEST_CHAINLINK_DEPLOY_DIR) && \
	solana account $(CHAINLINK_OTHERV1) --output json > $(TEST_CHAINLINK_DEPLOY_DIR)/other_v1.json

# The chainlink-build-mini-v* tasks run fresh anytime in order to pick up changes
# to the mini-program code.
chainlink-build-mini-v2:
	mkdir -p $(TEST_CHAINLINK_DEPLOY_DIR)/miniv2 && \
	MINI_PROGRAM_ID=$(CHAINLINK_MINIV2) \
	cargo build-sbf \
		--manifest-path $(TEST_CHAINLINK_MINI_PROGRAM_DIR)Cargo.toml \
		--sbf-out-dir $(TEST_CHAINLINK_DEPLOY_DIR)/miniv2 && \
	node $(TEST_CHAINLINK_DIR)/scripts/miniv2-json-from-so.js \
		$(TEST_CHAINLINK_DEPLOY_DIR)/miniv2/program_mini.so \
		$(TEST_CHAINLINK_DEPLOY_DIR)/miniv2/program_mini.json

chainlink-build-mini-v3:
	mkdir -p $(TEST_CHAINLINK_DEPLOY_DIR)/miniv3 && \
	MINI_PROGRAM_ID=$(CHAINLINK_MINIV3) \
	cargo build-sbf \
		--manifest-path $(TEST_CHAINLINK_MINI_PROGRAM_DIR)Cargo.toml \
		--sbf-out-dir $(TEST_CHAINLINK_DEPLOY_DIR)/miniv3

.PHONY: chainlink-prep-memo-v1 chainlink-prep-memo-v2 chainlink-prep-other-v1 chainlink-build-mini-v2 chainlink-build-mini-v3 chainlink-chainlink-prep-programs chainlink-list chainlink-dirs
