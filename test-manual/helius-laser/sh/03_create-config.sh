#!/bin/bash

set -euo pipefail

# Check if either TRITON_API_KEY or HELIUS_API_KEY is set
if [[ -z "${HELIUS_API_KEY:-}" ]]; then
    echo "Error: HELIUS_API_KEY environment variable and optionally TRITON_API_KEY need to be set."
    exit 1
fi

# Get the directory where this script is located
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
CONFIG_DIR="$SCRIPT_DIR/../configs"
OUTPUT_FILE="/tmp/mb-test-laser.toml"

# Determine which template to use and which API key(s) to use
if [[ -n "${TRITON_API_KEY:-}" ]]; then
    TEMPLATE_FILE="$CONFIG_DIR/triton-config-template.toml"
    TRITON_API_KEY="$TRITON_API_KEY"
    HELIUS_API_KEY="$HELIUS_API_KEY"
    sed "s/<helius-api-key>/$HELIUS_API_KEY/g" "$TEMPLATE_FILE" |  \
    sed "s/<triton-api-key>/$TRITON_API_KEY/g"  \
    > "$OUTPUT_FILE"
else
    TEMPLATE_FILE="$CONFIG_DIR/helius-config-template.toml"
    HELIUS_API_KEY="$HELIUS_API_KEY"
    sed "s/<helius-api-key>/$HELIUS_API_KEY/g" "$TEMPLATE_FILE" \
    > "$OUTPUT_FILE"
fi

echo "Config file created at $OUTPUT_FILE"
