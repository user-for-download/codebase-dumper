Here is the updated `README.md` that perfectly matches the features, flags, and behaviors of the finalized **v0.3.1** code.

```markdown
# source-dumper

A high-performance CLI tool that aggregates source code files into formatted text chunks sized specifically for LLM context windows (ChatGPT, Claude, Gemini).

## Features

- 🌲 **Smart Project Tree**: Prepends a visual directory structure to the context.
- 🧹 **Comment Stripping**: Optionally removes comments and empty lines to save tokens.
- 🛡️ **Safety First**: Automatic binary file detection, symlink cycle protection, and a 100-level tree depth cap.
- ⚙️ **Configurable**: Use `.dumperrc` for project-specific defaults.
- 🎯 **Flexible Filtering**: Support for globs (`*.log`), brace expansion (`src/{api,cli}`), and specific file includes.

## Install

```bash
# From source
cargo install --path .
```

## Quick Start

```bash
# Dump all text files in current directory to dump/
source-dumper

# Dump only Rust files
source-dumper --type rs

# Dump PHP files with comments stripped
source-dumper --type php --clean

# Preview what would be processed without writing files
source-dumper --dry-run --verbose

# Initialize project-specific config
source-dumper init
```

## Usage

```bash
source-dumper [OPTIONS] [COMMAND]
```

### Commands

| Command  | Description                        |
|----------|------------------------------------|
| `init`   | Create a documented `.dumperrc`    |
| `config` | Show active configuration          |
| `run`    | Run the dumper (default)           |

### Options

| Flag                  | Description                              | Default           |
|-----------------------|------------------------------------------|--------------------|
| `--path <DIR>`        | Source directory to scan                 | `.`                |
| `--type <EXT>`        | Filter by extension (e.g., `rs`, `py`)   | All text files     |
| `--out <PATTERN>`     | Output path pattern                      | `dump/dump_*.txt`  |
| `--limit <N>`         | Max **bytes** per output file            | `110000`           |
| `--max-file-size <N>` | Skip files larger than N bytes           | `52428800` (50MB)  |
| `--clean`             | Remove comments and empty lines          | `false`            |
| `--no-clean-out`      | Don't wipe output dir before running     | `false`            |
| `--exclude <A,B>`     | Comma-separated exclude patterns         | (Sensible defaults)|
| `--include <A,B>`     | Comma-separated force-include patterns   |                    |
| `--progress`          | Show progress bar                        | `false`            |
| `--verbose` / `-v`    | Show skipped files and detailed logs     | `false`            |
| `--dry-run`           | Preview filenames without writing        | `false`            |
| `--no-tree`           | Skip the directory tree in output        | `false`            |
| `--tree-depth <N>`    | Max tree depth (Hard cap: 100)           | `20`               |
| `--show-size`         | Show file sizes in the project tree      | `false`            |
| `--hidden`            | Include hidden files (starting with `.`) | `false`            |

## Output Pattern

The `--out` pattern determines how chunks are named:

| Placeholder   | Replaced with                  |
|---------------|--------------------------------|
| `*`           | Chunk number (1, 2, 3...)      |
| `{index}`     | Chunk number                   |
| `{type}`      | The extension used (or `all`)  |

**Examples:**
- `--out "dump/dump_*.txt"` → `dump/dump_1.txt`
- `--out "out/{type}_{index}.txt"` → `out/rs_1.txt`

## Configuration (`.dumperrc`)

Run `source-dumper init` to create a config file. CLI arguments always override `.dumperrc` values.

```ini
# .dumperrc example
path = .
type = ts
limit = 110000
exclude = target, dist, *.log, src/{tests,benchmarks}
include = README.md, package.json
progress = true
clean = true
```

## Advanced Filtering

### Exclude Patterns
Matches are checked against the **relative path** from your source directory.
- **Globs**: `*.log` matches any log file. `?` matches a single character.
- **Brace Expansion**: `src/{api,cli}` expands to `src/api` and `src/cli`.
- **Boundaries**: A pattern like `dist` will match the folder `dist/` but **not** `dist-assets/`.

### Include Patterns
Force-include specific files that would otherwise be filtered out by `--type` or `--exclude`:
```bash
source-dumper --type rs --include Cargo.toml,Dockerfile,README.md
```

## Comment Cleaning

When `--clean` is enabled, `source-dumper` uses language-specific regex to strip comments while preserving string literals:

| Language Style | Applied to Extensions |
|----------------|-----------------------|
| **C-Style**    | `rs`, `js`, `ts`, `go`, `cpp`, `c`, `java`, `swift`, etc. |
| **Script**     | `py`, `rb`, `sh`, `yml`, `toml`, `env`, `Dockerfile`, `Makefile` |
| **PHP**        | `php` |
| **HTML**       | `html`, `xml`, `svg`, `vue` |
| **SQL**        | `sql` |

## Safety Features

- **Binary Detection**: Automatically skips non-text files by checking for NUL bytes in the first 1KB.
- **UTF-8 Only**: Skips files with invalid UTF-8 encoding (and logs them in `--verbose` mode).
- **Symlink Protection**: Detects and breaks infinite recursion loops caused by circular symlinks.
- **No `rm -rf`**: The output cleaner only deletes files matching your `--out` pattern; it will never delete unrelated files or directories.

