#!/usr/bin/env bash
# Prints the release version: `[workspace.package] version` in the root Cargo.toml,
# from the working tree or from the given git revision.
# Release tags are `v` followed by this version.

set -euo pipefail

root="$(dirname "$0")/../.."
revision="${1:-}"

if [[ -n "$revision" ]]; then
  manifest="$(git -C "$root" show "${revision}:Cargo.toml")"
else
  manifest="$(cat "$root/Cargo.toml")"
fi

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
  ' <<< "$manifest"
)"

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Invalid workspace version in ${revision:-the working tree}'s Cargo.toml: \"$version\"" >&2
  exit 1
fi

echo "$version"
