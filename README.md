# spindle

AI-powered file organizer that uses content analysis to intelligently group and sort files.

## What it does

Spindle scans directories, fingerprints files (BLAKE3 + perceptual hashing), detects duplicates, and uses Claude to understand file contents and group them into logical categories. An interactive TUI lets you review and approve the proposed organization before anything moves.

Spindle remembers what it has already organized. A persistent ledger (path + content hash) keeps previously-filed files from being re-proposed on later runs, while new files are still compared against that history — flagged when they're byte-identical to organized content, and sorted into existing group folders instead of near-duplicate ones. Claude results are cached by content hash: per-file descriptions are keyed by each file's hash, and the grouping step is keyed by the whole set of file hashes (plus existing folder labels). Re-running over an unchanged set sends zero tokens — everything is read from the cache.

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

# Verbose output
spindle -v /path/to/folder

# Ignore the organized-files ledger for this run (don't skip or record)
spindle --no-ledger /path/to/folder

# Use a specific ledger file instead of the global default
spindle --ledger ./my-ledger.json /path/to/folder
```

## Configuration

Spindle needs an Anthropic API key for content analysis (skip with `--no-ai`
or `--dupes-only`). Provide it any of these ways (first match wins):

```bash
# 1. CLI flag
spindle --api-key sk-ant-... /path/to/folder

# 2. Environment variable (or a .env file in the working directory)
export ANTHROPIC_API_KEY=sk-ant-...

# 3. Config file (~/.config/spindle/config.toml)
# [ai]
# api_key = "sk-ant-..."
```

To route through a proxy, set `ANTHROPIC_BASE_URL`, pass `--api-base-url`,
or set `base_url` under `[ai]` in the config file.

## Development

```bash
just build          # cargo build
just test           # cargo test
just lint           # clippy + fmt check
just release-check  # full quality gate
```

## License

MIT
