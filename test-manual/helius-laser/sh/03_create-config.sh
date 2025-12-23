#!/bin/bash

set -euo pipefail

# Check if either TRITON_API_KEY or HELIUS_API_KEY is set
if [[ -z "${TRITON_API_KEY:-}" ]] && [[ -z "${HELIUS_API_KEY:-}" ]]; then
    echo "Error: Neither TRITON_API_KEY nor HELIUS_API_KEY environment variable is set."
    exit 1
fi

# Get the directory where this script is located
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
CONFIG_DIR="$SCRIPT_DIR/../configs"
OUTPUT_FILE="/tmp/mb-test-laser.toml"

# Determine which template to use and which API key to use
if [[ -n "${TRITON_API_KEY:-}" ]]; then
    TEMPLATE_FILE="$CONFIG_DIR/triton-config-template.toml"
    API_KEY="$TRITON_API_KEY"
else
    TEMPLATE_FILE="$CONFIG_DIR/helius-config-template.toml"
    API_KEY="$HELIUS_API_KEY"
fi

# Copy template and replace placeholder
sed "s/<api-key>/$API_KEY/g" "$TEMPLATE_FILE" > "$OUTPUT_FILE"

echo "Config file created at $OUTPUT_FILE"
