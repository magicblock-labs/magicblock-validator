#!/usr/bin/env bash

# NOTE: either source this script or assign the printed value
KEYPAIR_PATH=$(solana config get | grep "Keypair Path:" | cut -d: -f2 | xargs)
export KEYPAIR_PATH
echo $KEYPAIR_PATH
