#!/bin/bash

set -e

# Step 1: Read the version from Cargo.toml
version=$(grep '^version = ' ../Cargo.toml | head -n 1 | sed 's/version = "\(.*\)"/\1/')

if [ -z "$version" ]; then
    echo "Version not found in Cargo.toml"
    exit 1
fi

echo "Aligning for version: $version"

# GNU/BSD compat
sedi=(-i'')
case "$(uname)" in
  # For macOS, use two parameters
  Darwin*) sedi=(-i '')
esac

# Update the main package version in packages/npm-package/package.json.tmpl
jq --arg version "$version" '.version = $version' packages/npm-package/package.json.tmpl > temp.json && mv temp.json packages/npm-package/package.json.tmpl

# Update the main package version and only ephemeral-validator optionalDependencies versions in packages/npm-package/package.json
jq --arg version "$version" '(.version = $version) | (.optionalDependencies |= with_entries(if (.key | contains("ephemeral-validator")) then .value = $version else . end))' packages/npm-package/package.json > temp.json && mv temp.json packages/npm-package/package.json

# Check and update vrf-oracle and rpc-router to latest NPM versions
echo "Checking latest NPM versions for vrf-oracle and rpc-router..."

# Get latest versions from NPM
vrf_latest=$(npm view @magicblock-labs/vrf-oracle-linux-x64 version 2>/dev/null || echo "")
router_latest=$(npm view @magicblock-labs/rpc-router-linux-x64 version 2>/dev/null || echo "")

if [ -n "$vrf_latest" ]; then
    echo "Updating vrf-oracle dependencies to version: $vrf_latest"
    jq --arg version "$vrf_latest" '.optionalDependencies |= with_entries(if (.key | contains("vrf-oracle")) then .value = $version else . end)' packages/npm-package/package.json > temp.json && mv temp.json packages/npm-package/package.json
else
    echo "Warning: Could not fetch latest vrf-oracle version from NPM"
fi

if [ -n "$router_latest" ]; then
    echo "Updating rpc-router dependencies to version: $router_latest"
    jq --arg version "$router_latest" '.optionalDependencies |= with_entries(if (.key | contains("rpc-router")) then .value = $version else . end)' packages/npm-package/package.json > temp.json && mv temp.json packages/npm-package/package.json
else
    echo "Warning: Could not fetch latest rpc-router version from NPM"
fi

# Check if the any changes have been made to the specified files, if running with --check
if [[ "$1" == "--check" ]]; then
    files_to_check=(
        "packages/npm-package/package.json.tmpl"
        "packages/npm-package/package.json"
    )

    for file in "${files_to_check[@]}"; do
        # Check if the file has changed from the previous commit
        if git diff --name-only | grep -q "$file"; then
            echo "Error: version not aligned for $file. Align the version, commit and try again."
            exit 1
        fi
    done
    exit 0
fi