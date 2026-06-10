# Spindle — project command runner
# https://github.com/casey/just

set dotenv-load := true

version := `grep '^version' Cargo.toml | head -1 | cut -d'"' -f2`
bin := "spindle"

# ─── Default ──────────────────────────────────────────────────────

default:
    @just --list --unsorted

# ─── Development ──────────────────────────────────────────────────

# Build debug binary
build:
    cargo build

# Build optimized release binary
build-release:
    cargo build --release

# Build with optional video feature flag
build-video:
    cargo build --features video

# Install spindle to ~/.cargo/bin
install:
    cargo install --path .

# Run spindle with arguments (e.g. `just run -- --dry-run`)
run *ARGS:
    cargo run -- {{ ARGS }}

# Run without moving files (preview pipeline)
dry-run *ARGS:
    cargo run -- --dry-run {{ ARGS }}

# Watch for changes and rebuild on save
watch:
    cargo watch -x check -x 'test -- --nocapture'

# Check compilation without producing binaries
check:
    cargo check --all-targets --all-features

# Open spindle in Cursor classic editor (full LSP)
edit-classic:
    cursor --classic {{ justfile_directory() }}

# Diagnose rust-analyzer / Cursor LSP wiring
ra-doctor:
    #!/usr/bin/env bash
    set -euo pipefail
    BUNDLED="/home/zrk/.cursor/extensions/rust-lang.rust-analyzer-0.3.2921-linux-x64/server/rust-analyzer"
    echo "=== rust-analyzer doctor ==="
    echo "Extension server: $BUNDLED"
    if [[ -x "$BUNDLED" ]]; then
        "$BUNDLED" --version
    else
        echo "MISSING: install rust-analyzer extension in Cursor"
        exit 1
    fi
    echo
    echo "Running analysis-stats (proves RA can index this crate)..."
    timeout 60 "$BUNDLED" analysis-stats . | tail -5
    echo
    echo "=== Cursor Agents window (likely root cause) ==="
    echo "Go-to-definition does NOT work in Agent layout file tabs."
    echo "Fix: open this repo in the classic editor instead:"
    echo "  just edit-classic"
    echo
    echo "If classic editor still fails:"
    echo "  Rust Analyzer: Restart server"
    echo "  Developer: Reload Window"

# Kill bundled rust-analyzer zombies; reload Cursor window after
ra-restart:
    #!/usr/bin/env bash
    set -euo pipefail
    pkill -f '/home/zrk/.cursor/extensions/rust-lang.rust-analyzer-.*/server/rust-analyzer' \
      2>/dev/null || true
    echo "Bundled rust-analyzer stopped (if any)."
    echo "In Cursor: Rust Analyzer: Restart server, then Reload Window."

# ─── Spindle Commands ────────────────────────────────────────────

# Find duplicates only (TUI review, no full organize run)
dupes-only *ARGS:
    cargo run -- --dupes-only {{ ARGS }}

# Organize without AI classification
no-ai *ARGS:
    cargo run -- --no-ai {{ ARGS }}

# Undo last operation from output dir undo log
undo *ARGS:
    cargo run -- --undo {{ ARGS }}

# Build and run debug binary
bar *ARGS:
    cargo build
    ./target/debug/{{ bin }} {{ ARGS }}

# Copy .env.example to .env when missing
env-setup:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -f .env ]]; then
        echo ".env already exists"
    else
        cp .env.example .env
        echo "Created .env from .env.example"
    fi

# Show CLI help
help:
    cargo run -- --help

# ─── Testing ─────────────────────────────────────────────────────

# Run all unit tests
test:
    cargo test

# Run a single test by name
test-one NAME:
    cargo test {{ NAME }} -- --nocapture

# Run tests with stdout visible
test-verbose:
    cargo test -- --nocapture

# Run integration test crate
test-integration:
    cargo test --test integration_test

# Run tests with all features enabled
test-all:
    cargo test --all-features

# Generate HTML coverage report (cargo-llvm-cov)
test-coverage:
    cargo llvm-cov --html

# Open HTML coverage report in browser
test-coverage-report: test-coverage
    #!/usr/bin/env bash
    set -euo pipefail
    REPORT="target/llvm-cov/html/index.html"
    if [[ ! -f "$REPORT" ]]; then
        echo "Missing $REPORT — run just test-coverage first"
        exit 1
    fi
    xdg-open "$REPORT" 2>/dev/null \
      || echo "Report: $REPORT"

# Fail if line coverage is below threshold (percent)
test-coverage-check THRESHOLD="80":
    cargo llvm-cov --fail-under-lines {{ THRESHOLD }}

# ─── Linting & Formatting ───────────────────────────────────────

# Format all source files
fmt:
    cargo fmt --all

# Check formatting without modifying files
fmt-check:
    cargo fmt --all -- --check

# Run clippy with warnings-as-errors
clippy:
    cargo clippy --all-targets --all-features -- -D warnings

# Run clippy and auto-fix what it can
clippy-fix:
    cargo clippy --all-targets --all-features --fix --allow-dirty -- \
      -D warnings

# Lint everything (format check + clippy)
lint: fmt-check clippy

# Fix everything (format + clippy auto-fix)
fix: fmt clippy-fix

# ─── Documentation ──────────────────────────────────────────────

# Build rustdoc
doc:
    cargo doc --no-deps --all-features

# Build and open rustdoc in browser
doc-open:
    cargo doc --no-deps --all-features --open

# ─── Security & Dependencies ────────────────────────────────────

# Audit dependencies for known vulnerabilities
audit:
    cargo audit

# Show outdated dependencies
outdated:
    cargo outdated

# Show the full dependency tree
deps:
    cargo tree

# Show duplicate dependencies
deps-dupes:
    cargo tree -d

# Update all dependencies to latest compatible versions
deps-update:
    cargo update

# Verify crates.io publish metadata without uploading
publish-dry-run:
    cargo publish --dry-run

# ─── Pre-commit & CI ────────────────────────────────────────────

# Quick pre-commit checks (format + clippy + check)
pre-commit: fmt clippy check

# Full local CI pipeline (lint + tests)
ci-local: lint test

# Alias for release quality gate
check-all: release-check

# ─── Release ─────────────────────────────────────────────────────

# Release quality gate (fmt + clippy + test)
release-check:
    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test

# Preview what a release would do without changing anything
release-dry-run LEVEL:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! "{{ LEVEL }}" =~ ^(patch|minor|major)$ ]]; then
        echo "Usage: just release-dry-run patch|minor|major"; exit 1
    fi
    CURRENT=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
    echo "Current version: $CURRENT"
    echo "Bump level: {{ LEVEL }}"
    just release-check
    echo ""
    echo "All checks passed. Run: just release {{ LEVEL }}"

# Bump version, create release branch + PR (requires: cargo-set-version, gh)
release LEVEL: release-check
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! "{{ LEVEL }}" =~ ^(patch|minor|major)$ ]]; then
        echo "Usage: just release patch|minor|major"; exit 1
    fi
    if [[ -n "$(git status --porcelain)" ]]; then
        echo "Error: dirty working tree"; exit 1
    fi
    BRANCH=$(git rev-parse --abbrev-ref HEAD)
    if [[ "$BRANCH" != "main" ]]; then
        echo "Error: must be on main (currently on $BRANCH)"; exit 1
    fi
    git pull --ff-only origin main
    cargo set-version --bump {{ LEVEL }}
    cargo check --quiet
    VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
    git checkout -b "release/v${VERSION}"
    git add Cargo.toml Cargo.lock
    git commit -m "release: v${VERSION}"
    git push -u origin "release/v${VERSION}"
    gh pr create \
        --title "release: v${VERSION}" \
        --body "Bump to v${VERSION} ({{ LEVEL }} release)" \
        --base main

    echo "Waiting for CI checks to appear..."
    for i in $(seq 1 30); do
        if gh pr checks --json name 2>/dev/null | grep -q name; then break; fi
        sleep 2
    done
    echo "Watching CI checks..."
    gh pr checks --watch --fail-fast

    echo "CI passed. Merging..."
    gh pr merge --squash --delete-branch

    git checkout main
    git pull --ff-only origin main

    echo "Watching release workflow..."
    gh run watch

# ─── Cleanup ─────────────────────────────────────────────────────

# Remove build artifacts
clean:
    cargo clean

# Remove build artifacts and coverage reports
clean-all: clean
    rm -rf target/llvm-cov coverage dist

# ─── Project Info ────────────────────────────────────────────────

# Show project and toolchain versions
info:
    @echo "Spindle v{{ version }} ({{ bin }})"
    @echo ""
    @echo "Toolchain"
    @echo "  rustc:  $(rustc --version)"
    @echo "  cargo:  $(cargo --version)"
    @echo "  just:   $(just --version)"
    @echo ""
    @echo "Dev Tools"
    @echo "  cargo-set-version: $(cargo set-version --version 2>/dev/null || echo 'not installed')"
    @echo "  cargo-audit:       $(cargo audit --version 2>/dev/null || echo 'not installed')"
    @echo "  cargo-outdated:    $(cargo outdated --version 2>/dev/null || echo 'not installed')"
    @echo "  cargo-watch:       $(cargo watch --version 2>/dev/null || echo 'not installed')"
    @echo "  cargo-llvm-cov:    $(cargo llvm-cov --version 2>/dev/null || echo 'not installed')"

# Show lines of Rust source
loc:
    @echo "Source:"
    @find src tests -name '*.rs' 2>/dev/null | xargs wc -l | tail -1

# Show disk usage of build artifacts and caches
cache-status:
    @echo "Disk Usage"
    @echo "  target/:           $(du -sh target 2>/dev/null | cut -f1 || echo 'n/a')"
    @echo "  target/llvm-cov/:  $(du -sh target/llvm-cov 2>/dev/null | cut -f1 || echo 'n/a')"
    @echo "  ~/.cargo/registry: $(du -sh ~/.cargo/registry 2>/dev/null | cut -f1 || echo 'n/a')"

# Install common development tools
install-tools:
    cargo install cargo-set-version cargo-audit cargo-outdated cargo-watch
    cargo install cargo-llvm-cov
