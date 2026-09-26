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
# Scan and propose a plan; nothing moves until you approve it
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

# Only photos and videos
spindle --type photo,video /path/to/folder

# Stop before spending more than a dollar on analysis
spindle --max-cost 1.00 /path/to/folder
```

## Undoing a run

Every executed run is journalled, and nothing is deleted outright:
files spindle would remove are staged in a trash directory instead.
Both live under your data directory (`~/.local/share/spindle` on
Linux).

```bash
# What can be undone, newest first
spindle --list-undo

# Put the most recent run back
spindle --undo

# Put a specific run back
spindle --undo-run <RUN_ID>
```

Undo moves files back where they came from and restores deletions out
of the staged trash. If some files cannot be restored — you moved one
yourself since the run, say — spindle reports them and keeps the
journal, so you can clear the obstruction and undo again.

Staged deletions still occupy disk. Reclaim the space once you are
sure you want them gone:

```bash
# Empty the staged trash
spindle --purge

# Only the parts older than a week
spindle --purge --older-than 7
```

## Options

`spindle --help` is the full list. The ones worth knowing:

| Flag | What it does |
| --- | --- |
| `-n, --dry-run` | Print what you approved on the review screen and exit without moving anything. |
| `-y, --yes` | Execute without the review screen. Undoable. |
| `--json` | Print the plan as JSON and exit; no terminal needed. |
| `-o, --output <DIR>` | Where organized files go. |
| `-t, --type <LIST>` | Only these categories: image, video, audio, document, archive, installer (aliases: photo, movie, music, pdf, zip, app). Comma-separated. |
| `--max-cost <USD>` | Stop before spending more than this on analysis. |
| `--max-files <N>` | Cap how many files go to the model. |
| `--batch` | Use the Batch API: half the price, minutes to hours. |
| `--dupes-only` | Exact duplicates only — no AI, no grouping, no review screen. |
| `--no-ai` | Skip analysis entirely; hash-based dedup only. |
| `--include-trash` | Scan trash and recycle-bin folders too. |
| `-c, --config <PATH>` | Config file to use instead of the default. |
| `-v, --verbose` | Repeatable: `-v`, `-vv`, `-vvv`. |

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

### Colours

The review screen ships five themes, chosen with `--theme` or `theme` under
`[general]` in the config file:

| Name | |
|---|---|
| `auto` | the default: follow the desktop theme if there is one, else `spindle` |
| `spindle` | violet accents over whatever background your terminal already has |
| `gloss` | hot pink on a deep void |
| `deep-night` | midnight navy with a periwinkle accent |
| `light-luxury` | ivory paper and brass, for terminals that are actually light |

`auto` and `spindle` paint no background of their own — matching your terminal
means staying out of its way — while the other three bring their own surface.
Every built-in is checked against WCAG contrast ratios by a test, so no theme
can ship a label you cannot read.

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

`Tab` cycles the focus through all three columns, so when a description runs
past the bottom of the pane you can Tab into it and scroll with `j`/`k`, the
arrows, `PageUp`/`PageDown` or `Home`/`End`. From any column, `[` and `]`
scroll it without moving the focus.

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

### Eval

Grouping quality is measured, not argued about. Four fixtures under
`tests/fixtures/` are trees of realistic files belonging to one
invented person, each with an `expected.toml` answer key naming the
folder every file should land in:

| Fixture | What it is for |
| --- | --- |
| `organize` | The baseline. A few dozen files across the usual areas. |
| `organize-large` | Around 140 files, ambiguous items, office and ebook formats, exact and near duplicates. |
| `organize-second-run` | A dozen folders already exist from a previous run; most incoming files belong in them. Measures label reuse. |
| `organize-granularity` | Clusters big enough that splitting them is visibly wrong. Measures folder shape. |

```bash
just eval         # follows your ANTHROPIC_* environment, proxy included
just eval-direct  # same, pinned at api.anthropic.com
```

Both call the real API and cost money, so the tests are `#[ignore]`d
and gated on `SPINDLE_EVAL=1`; a plain `just test` never touches the
network. Per-file descriptions are cached under `target/eval-cache`,
so re-runs only pay for the grouping call.

Each run prints a report:

| Line | |
| --- | --- |
| `placed` | Share of the answer key's files the plan placed at all. |
| `pairwise f1` | Whether files that belong together ended up together, and apart otherwise. Precision and recall are printed beside it. |
| `top-level accuracy` | Whether each file reached an acceptable top-level area. The key may name more than one. |
| `hygiene` | Groups that are outright malformed: named for a file type, past the depth cap, holding one file. |
| `granularity` | The shape of the result — how many files landed somewhere too thin to be worth opening. |
| `label reuse` | Of files whose folder already existed, how many were filed under exactly that label. Second-run fixtures only. |
| `alternatives offered` | Whether the review pane's ALSO FITS list contains a folder the key would have accepted. Reported only. |
| `composite` | One number, weighing the first five. Reported-only lines are left out of it on purpose. |

A fixture is graded against **its own ceiling** — what its answer key
scores when every file lands exactly where the key says — minus a
named slack, rather than against an absolute number. Four fixtures
have four different ceilings, and a fixture that gains a file moves
its own.

That makes editing a fixture a change to the grading, so two guards
sit in `tests/eval_grouping.rs` and run without an API key:

- `MIN_CEILING` records each fixture's ceiling. Splitting one expected
  folder into two thin ones drops the ceiling and every floor derived
  from it; the recorded value has to be changed by hand, deliberately.
- The answer keys are run through the validator. A folder it would
  merge away or rename is a folder no run can produce, so asking for
  one is a bug in the fixture, and the guard says which label and what
  the validator does to it.

Both fail on `just test`. If you add a file to a fixture, run it.

## License

MIT
