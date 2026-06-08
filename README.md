# spindle

AI-powered file organizer that uses content analysis to intelligently group and sort files.

## What it does

Spindle scans directories, fingerprints files (BLAKE3 + perceptual hashing), detects duplicates, and uses Claude to understand file contents and group them into logical categories. An interactive TUI lets you review and approve the proposed organization before anything moves.

## Install

```bash
cargo install spindle
```

Or download a binary from [Releases](https://github.com/z19r/spindle/releases).

## Usage

```bash
# Scan and organize (dry-run by default)
spindle /path/to/messy/folder

# Find duplicates only
spindle --dupes-only /path/to/folder

# Describe files without grouping
spindle --describe-only /path/to/folder

# Verbose output
spindle -v /path/to/folder
```

## Configuration

Copy `.env.example` to `.env` and set your Anthropic API key:

```bash
cp .env.example .env
```

## Development

```bash
just build          # cargo build
just test           # cargo test
just lint           # clippy + fmt check
just release-check  # full quality gate
```

## License

MIT
