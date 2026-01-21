#!/usr/bin/env bash

if [ -n "$1" ]; then
	KEYPAIR_PATH="$1"
elif [ -n "$KEYPAIR_PATH" ]; then
	KEYPAIR_PATH="$KEYPAIR_PATH"
else
	echo "Error: No keypair path provided. Please provide a keypair path as an argument or set the KEYPAIR_PATH environment variable."
	exit 1
fi

PUBKEY=$(solana-keygen pubkey "$KEYPAIR_PATH")
echo "Airdropping 2 SOL to $PUBKEY..."
solana airdrop --url devnet 2 "$PUBKEY"
