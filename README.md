# Source Dumper

Aggregate source files into chunks for LLM context windows.

## Installation

```bash
# Linux/macOS
chmod +x source-dumper-*
./source-dumper-linux-x64 --help
```

### Build from Source

```bash
cargo build --release
```

## Quick Start

```bash
# Initialize config
source-dumper init

# Dump PHP files
source-dumper --type php

# Dump Rust files with cleaning
source-dumper --type rs --clean --progress

# Dry run
source-dumper --type js --dry-run --verbose
```

## Usage

```bash
source-dumper [OPTIONS] --type <EXTENSION>
```

## Options

| Flag | Description | Default |
|------|-------------|---------|
| `--type` | File extension to process | required |
| `--path` | Source directory | `.` |
| `--out` | Output pattern | `dump/dump_*.txt` |
| `--format` | Output format (plain/markdown/json) | `plain` |
| `--limit` | Characters per chunk | `110000` |
| `--clean` | Remove comments/empty lines | `false` |
| `--exclude` | Exclude paths (comma-separated) | - |
| `--include` | Include specific files | - |
| `--progress` | Show progress bar | `false` |
| `--verbose` | Verbose output | `false` |
| `--dry-run` | Preview without writing | `false` |
| `--no-tree` | Skip tree view | `false` |
| `--tree-depth` | Max tree depth | unlimited |
| `--hidden` | Include hidden files | `false` |

## Config File

Create `.dumperrc` in project root:

```ini
type = php
out = dump/dump_*.txt
limit = 110000
clean = false
exclude = vendor, node_modules, .git, storage, cache
include = .env.example, docker-compose.yml
```

## Examples

```bash
# PHP project
source-dumper --type php --exclude vendor,storage --clean

# JavaScript with specific includes
source-dumper --type js --include package.json,tsconfig.json

# Markdown output for documentation
source-dumper --type py --format markdown --out docs/source_*.md

# JSON output
source-dumper --type rs --format json --out dump/code.json

# Large project with progress
source-dumper --type java --progress --limit 50000
```

## Output

```
dump/
├── dump_1.txt    # First chunk
├── dump_2.txt    # Second chunk
└── dump_3.txt    # Third chunk
```

Each chunk contains:
- Project tree structure
- File contents with headers

## License

MIT
