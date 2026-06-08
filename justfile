# Rust project justfile
default:
    @echo "Available Rust commands:"
    @just --list

# Run the application
dry-run:
    @echo "Running Rust application..."
    @cargo run -- --dry-run

# Build the application
build:
    @echo "Building Rust application..."
    @cargo build

# Build for release
build-release:
    @echo "Building Rust application for release..."
    @cargo build --release

# Run tests
test:
    @echo "Running Rust tests..."
    @cargo test

# Run tests with coverage
test-coverage:
    @echo "Running Rust tests with coverage..."
    @cargo test --no-run
    @cargo llvm-cov --html

# Run linting
lint:
    @echo "Running Clippy..."
    @cargo clippy

# Format code
fmt:
    @echo "Formatting Rust code..."
    @cargo fmt

# Install dependencies
install:
    @echo "Installing Rust dependencies..."
    @cargo build

bar:
  @just build
  ./target/debug/spindel
