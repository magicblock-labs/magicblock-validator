#!/usr/bin/env bash
# Prints the release version: `[workspace.package] version` in the root Cargo.toml.
# Release tags are `v` followed by this version.

set -euo pipefail

manifest="$(dirname "$0")/../../Cargo.toml"

version="$(
  awk '
    $0 == "[workspace.package]" { in_section = 1; next }
    in_section && /^\[/ { exit }
    in_section && /^version = "/ {
      value = $0
      sub(/^version = "/, "", value)
      sub(/"$/, "", value)
      print value
      exit
    }
  ' "$manifest"
)"

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Invalid workspace version in $manifest: \"$version\"" >&2
  exit 1
fi

echo "$version"
