default:
    @just --list

# Run the application (dry-run)
dry-run *ARGS:
    cargo run -- --dry-run {{ARGS}}

# Build
build:
    cargo build

# Build for release
build-release:
    cargo build --release

# Run tests
test:
    cargo test

# Run tests with coverage
test-coverage:
    cargo llvm-cov --html

# Lint (clippy + fmt check)
lint:
    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings

# Format code
fmt:
    cargo fmt

# Release quality gate (fmt + clippy + test)
release-check:
    cargo fmt --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo test

# Preview what a release would do without changing anything
release-dry-run LEVEL:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! "{{LEVEL}}" =~ ^(patch|minor|major)$ ]]; then
        echo "Usage: just release-dry-run patch|minor|major"; exit 1
    fi
    CURRENT=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
    echo "Current version: $CURRENT"
    echo "Bump level: {{LEVEL}}"
    just release-check
    echo ""
    echo "All checks passed. Run: just release {{LEVEL}}"

# Bump version, create release branch + PR (requires: cargo-set-version, gh)
release LEVEL: release-check
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! "{{LEVEL}}" =~ ^(patch|minor|major)$ ]]; then
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
    cargo set-version --bump {{LEVEL}}
    cargo check --quiet
    VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
    git checkout -b "release/v${VERSION}"
    git add Cargo.toml Cargo.lock
    git commit -m "release: v${VERSION}"
    git push -u origin "release/v${VERSION}"
    gh pr create \
        --title "release: v${VERSION}" \
        --body "Bump to v${VERSION} ({{LEVEL}} release)" \
        --base main
    echo ""
    echo "PR created. Next steps:"
    echo "  gh pr checks           # watch CI"
    echo "  gh pr merge --squash --delete-branch"
    echo "  gh run watch           # watch release workflow"
