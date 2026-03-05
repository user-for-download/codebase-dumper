# source-dumper

CLI tool that aggregates source files into text chunks sized for LLM context windows.

## Install

```bash
cargo install --path .
```

## Quick Start

```bash
# Dump all text files in current directory
source-dumper

# Dump only Rust files
source-dumper --type rs

# Dump PHP files with comments stripped
source-dumper --type php --clean

# Preview what would be processed
source-dumper --dry-run --verbose

# Initialize config file
source-dumper init
```

## Usage

```
source-dumper [OPTIONS] [COMMAND]
```

### Commands

| Command  | Description                        |
|----------|------------------------------------|
| `init`   | Create `.dumperrc` config file     |
| `config` | Show current configuration         |
| `run`    | Run the dumper (default if omitted)|

### Options

| Flag                  | Description                              | Default           |
|-----------------------|------------------------------------------|--------------------|
| `--type <EXT>`        | File extension filter (empty = all text) | all text files     |
| `--path <DIR>`        | Source directory                          | `.`                |
| `--out <PATTERN>`     | Output path pattern                      | `dump/dump_*.txt`  |
| `--limit <N>`         | Max characters per output file           | `110000`           |
| `--max-file-size <N>` | Skip files larger than N bytes           | `52428800` (50MB)  |
| `--clean`             | Remove comments and empty lines          | `false`            |
| `--exclude <A,B,C>`   | Exclude directories/files                | see below          |
| `--exclude-file <F>`  | Read exclude patterns from file          |                    |
| `--include <A,B,C>`   | Include specific files                   |                    |
| `--include-file <F>`  | Read include patterns from file          |                    |
| `--progress`          | Show progress bar                        | `false`            |
| `--verbose` / `-v`    | Verbose output                           | `false`            |
| `--dry-run`           | Preview without writing                  | `false`            |
| `--no-tree`           | Skip tree view in output                 | `false`            |
| `--tree-depth <N>`    | Max tree depth (max 100)                 | unlimited          |
| `--show-size`         | Show file sizes in tree                  | `false`            |
| `--hidden`            | Include hidden files (dotfiles)          | `false`            |
| `--no-config`         | Ignore `.dumperrc`                       | `false`            |
| `--config <PATH>`     | Custom config file path                  | `.dumperrc`        |

## Output Pattern

The `--out` pattern supports placeholders:

| Placeholder   | Replaced with                  |
|---------------|--------------------------------|
| `*`           | Chunk number (1, 2, 3...)      |
| `{index}`     | Chunk number                   |
| `{type}`      | File extension (or `all`)      |

```bash
# Examples
--out "dump/dump_*.txt"           # dump/dump_1.txt, dump/dump_2.txt
--out "out/{type}_{index}.txt"    # out/rs_1.txt, out/rs_2.txt
```

## Config File

Create `.dumperrc` in your project root:

```bash
source-dumper init
```

Example `.dumperrc`:

```ini
# File type (empty = all text files)
type = rs

# Output settings
out = dump/dump_*.txt
limit = 110000

# Exclude patterns (comma-separated, supports brace expansion)
exclude = vendor, node_modules, .git, target, dist, build

# Include specific files regardless of type
include = Cargo.toml, Makefile, .env.example

# Processing
clean = false
progress = true
```

CLI arguments always override config file values.

## Exclude Patterns

Default excludes:
```
vendor, node_modules, .git, .idea, .vscode, storage, cache,
logs, tmp, temp, dist, build, coverage, target
```

Brace expansion supported:
```ini
exclude = src/{tests,fixtures,vendor}
# Expands to: src/tests, src/fixtures, src/vendor
```

Glob patterns:
```ini
exclude = *.log, *.lock
```

## Include Patterns

Force-include files that don't match `--type`:

```bash
source-dumper --type rs --include Cargo.toml,Dockerfile,README.md
```

Patterns match by:
- Exact filename: `Makefile`
- Path suffix: `src/config.toml`
- External path: `../shared/types.ts`

## Comment Cleaning

`--clean` removes comments based on file extension:

| Style    | Extensions                                       |
|----------|--------------------------------------------------|
| C-style  | rs, js, ts, go, java, cpp, c, cs, swift, kt, ... |
| Script   | py, rb, sh, yaml, toml, conf, Dockerfile, ...    |
| PHP      | php                                              |
| HTML     | html, xml, svg, vue, svelte                      |
| SQL      | sql, mysql, pgsql                                |

String literals are preserved — `"// not a comment"` stays intact.

## Binary Detection

Files are automatically checked for binary content (NUL bytes in first 8KB).
Binary files are skipped with a warning in verbose mode.

## Examples

```bash
# Dump entire project
source-dumper --progress

# Dump only Python files, clean comments, verbose
source-dumper --type py --clean --verbose --progress

# Dump with custom limit for smaller context windows
source-dumper --type js --limit 50000

# Dump with tree showing file sizes
source-dumper --type rs --show-size --tree-depth 5

# Dump specific directory excluding tests
source-dumper --path ./src --type ts --exclude tests,__mocks__

# Preview large project
source-dumper --dry-run --verbose --type go

# Use patterns from .gitignore-style file
source-dumper --type php --exclude-file .gitignore

# Include config files alongside source
source-dumper --type rs --include Cargo.toml,rust-toolchain.toml,.github/workflows/ci.yml
```

## Output Structure

Each output chunk contains:

```
PROJECT STRUCTURE: "/path/to/project"
==========================================
project/
├── src/
│   ├── main.rs
│   └── lib.rs
├── Cargo.toml
└── README.md

1 directories, 3 files, 15.20 KB
==========================================

--- FILE: src/main.rs ---
fn main() {
    println!("Hello, world!");
}

--- FILE: src/lib.rs ---
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}
```

Tree view appears in the first chunk only. Subsequent chunks contain file contents only.

## Safety

- **No `rm -rf`**: Previous output is cleaned by removing only files matching the output pattern
- **Binary skip**: Non-text files are detected and skipped automatically
- **Size limit**: Files exceeding `--max-file-size` are skipped
- **Symlink cycles**: Detected and reported in tree view
- **Tree depth cap**: Hard limit of 100 levels

## License

MIT
