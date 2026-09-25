# spindle

[![Monitored by Cooper&Wright](https://img.shields.io/badge/monitored%20by-Cooper%26Wright-8a6a3b?style=flat-square)](https://cooperwright.com)

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

# Print the proposed plan as JSON and exit (no terminal needed)
spindle --json /path/to/folder > plan.json

# Execute the proposed plan without the review screen (undoable)
spindle --yes /path/to/folder
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

### Top-level folders

Grouping runs in two stages: every file is first routed to one of your
top-level areas, then grouped into sub-folders within that area. The default
areas are Work, Personal, Finance, Legal, Health, Photos, Private, Media,
Software and Reference. Override them under `[taxonomy]`:

```toml
[taxonomy]
areas = [
  { name = "Work", description = "clients, projects, meetings" },
  { name = "Home", description = "family, house, bills, recipes" },
  { name = "Archive", description = "anything old worth keeping" },
]
```

An empty list (`areas = []`) groups in a single stage with no fixed top level.

Nudity and intimate content are described plainly, tagged `explicit` and
`private`, and routed to **Private**, never alongside family or trip photos.
When the model declines to describe an image at all, the file is filed as
"sensitive, declined to describe" and still lands in Private for you to
review, rather than falling back to a guess from the filename.

### The detail pane

Select a file in the review screen and the right-hand pane shows what Spindle
knows about it: the model's description and tags, then a **Metadata** table
read straight from the file. Photos get dimensions, when they were taken (with
the time of day), the place they were taken if they carry GPS, camera, lens and
exposure. Video and audio get duration, resolution, codecs and any embedded
title, artist or recording location (needs `ffprobe`, part of ffmpeg). PDFs get
page count, title and author; Office documents their title, author and
creation date; text files line and word counts; archives their entry count.
Place names come from an offline gazetteer, so nothing leaves your machine.

Below that, **Also fits** lists up to three other groups whose files share tags
with this one, with the shared tags and a couple of member filenames so you
can judge without navigating. Press `1`, `2` or `3` to move the file there.

### Learning from your review

When you rename a group, merge groups, or move files in the review screen and
then execute, Spindle remembers it in `~/.local/share/spindle/corrections.json`
(next to the ledger). The next run tells the model to use your names instead of
the ones it proposed before and to file those same files where you put them.
Pass `--no-corrections` to run without reading or recording them.

## Development

```bash
just build          # cargo build
just test           # cargo test
just lint           # clippy + fmt check
just release-check  # full quality gate
```

## License

MIT
