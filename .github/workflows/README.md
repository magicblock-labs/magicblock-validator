# CI Workflows Optimization

## Overview

The CI workflows have been optimized to reduce build time through:
- **Unified cache key**: All build/clippy/nextest steps share a single cache key
- **Strict job ordering**: Build → Clippy → Nextest (with proper dependencies)
- **Parallel format checks**: `fmt` runs in parallel with no cache overhead
- **Artifact reuse**: Integration tests reuse build cache from the main build step

## Workflows

### ci-test.yml (NEW - Primary)
Consolidates build, clippy, and unit tests with optimized caching and job ordering.

**Job sequence**:
```
build (ubuntu-latest-m)
  ├── clippy (depends on build) → runs on ubuntu-latest
  └── nextest (depends on build) → runs on ubuntu-latest-m

fmt (independent, ubuntu-latest) → runs in parallel with build
```

**Cache key**:
```
magicblock-validator-${HASH(Cargo.lock, test-integration/Cargo.lock)}-build-v001
```

**Benefits**:
- Single build cache shared across all steps
- Clippy runs only after build completes
- Nextest runs only after build completes
- Format checks run in parallel (no cache needed)
- Build cache is warm when clippy/nextest start

### ci-test-integration.yml (Updated)
Integration tests workflow (kept separate due to expensive matrix).

**Dependencies**:
- `run_integration_tests` depends on `build` job
- Reuses the same `BUILD_CACHE_KEY` as ci-test.yml
- Downloads pre-built test runner from build artifact

**Key changes**:
- Unified cache key across both workflows
- Simplified paths (removed magicblock-validator prefix)
- Proper artifact paths

### Deprecated Workflows
The following workflows are marked as deprecated but kept for backward compatibility:
- ci-lint.yml
- ci-test-unit.yml  
- ci-fmt.yml

These can be disabled in GitHub Actions settings if needed.

## Migration

If you were relying on individual workflows, they still exist but point to the new ci-test.yml:
- Lint checks → use ci-test.yml (clippy job)
- Unit tests → use ci-test.yml (nextest job)
- Format checks → use ci-test.yml (fmt job)

## Cache Strategy

All workflows use the same cache key derived from:
- `Cargo.lock` (main workspace)
- `test-integration/Cargo.lock` (integration test workspace)

This ensures:
1. Cache is invalidated when dependencies change
2. All jobs can reuse the same build artifacts
3. No redundant cache downloads/uploads
