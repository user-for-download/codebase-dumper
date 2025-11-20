# 🦀 Rust Codebase Dumper

A high-performance, CLI tool written in Rust designed to **prepare codebases for LLM context** (ChatGPT, Claude, etc.). It scans, cleans, and dumps source code into chunked text files while preserving directory structure.

## 🚀 Features

*   **Smart Cleaning:** Removes comments (`//`, `/* */`, `#`) and empty lines to save tokens.
*   **Recursive Scanning:** Blazing fast traversal using `WalkDir`.
*   **Smart Exclusion:** `exclude` matches path components, not just substrings (e.g., excluding `test` hides the folder `test/` but keeps `latest_test.php`).
*   **Brace Expansion:** Supports patterns like `--include src/{main,lib}.rs`.
*   **Chunking:** Splits output into multiple files (e.g., `dump_1.txt`, `dump_2.txt`) based on a token/character limit.
*   **Tree View:** Generates a visual directory tree at the top of the first output file.
*   **Pattern Files:** Can read include/exclude patterns from external files (like `.gitignore`).
*   **Safety:** Automatically wipes the output directory before starting, but includes safety checks to prevent accidental deletion of source code.

## 🛠 Installation

You need [Rust installed](https://www.rust-lang.org/tools/install).

```bash
# Build release binary
cargo build --release

# The binary will be in target/release/
./target/release/search_tool --help
```

## 📖 Usage

### Basic Syntax
```bash
./search_tool --type <EXT> [OPTIONS]
```

### Arguments

| Flag | Description | Default |
| :--- | :--- | :--- |
| `--type` | **Required.** Main extension to filter (e.g., `.php`, `.rs`, `.py`). | N/A |
| `--path` | Source directory to scan. | `.` (Current Dir) |
| `--out` | Output pattern. Must include `*` for numbering. | `dump/dump_*.txt` |
| `--clean` | Removes comments and collapses empty lines. | `false` |
| `--limit` | Character limit per output file. | `110000` |
| `--progress`| Shows a progress bar. | `false` |

### Filtering Options

| Flag | Description | Example |
| :--- | :--- | :--- |
| `--include`| Whitelist specific files or substrings. Supports brace expansion. | `--include .env,docker{file,-compose.yml}` |
| `--exclude`| Blacklist folders or path segments. | `--exclude vendor,node_modules,.git` |
| `--include-file`| Read include patterns from a file (one per line). | `--include-file priority_list.txt` |
| `--exclude-file`| Read exclude patterns from a file. | `--exclude-file .ignore` |

## 💡 Examples

### 1. The "LLM Context" Dump (PHP)
Scans `site03/app`, cleans comments, chunks output, and includes specific config files from the project root.

```bash
./search_tool \
  --path site03/app \
  --type .php \
  --clean \
  --out "context/php_code_*.txt" \
  --include site03/{.env,composer.json} \
  --exclude tests,Console \
  --progress
```

### 2. Rust Project (Default Defaults)
Scans the current directory for `.rs` files, outputting to `dump/`.

```bash
./search_tool --type .rs --clean --exclude target
```

### 3. Python Project (Script style)
The tool automatically detects `.py` files and switches to `#` based comment removal.

```bash
./search_tool \
  --path ./my_script \
  --type .py \
  --clean \
  --out "output/script_*.txt"
```

## ⚠️ Safety & Validation

1.  **Output Wiping:** The tool attempts to **DELETE** the folder specified in `--out` to ensure a clean dump.
    *   *Safety Check:* It calculates absolute paths. If the Source Path is inside the Output Path (or vice versa), it will **refuse** to delete the folder to protect your code.
2.  **Validation:** At the end of the run, it warns you if an `--include` pattern was not found (helps detect typos).

## 📄 Output Format

**File 1 (`dump_1.txt`) starts with:**
```text
PROJECT STRUCTURE: "site03/app"
==========================================
├── Controllers
│   └── UserController.php
├── Models
│   └── User.php
└── routes.php

==========================================

--- FILE: "site03/app/Models/User.php" ---
namespace App\Models;
class User {
    public function index() {
        return "Clean code without comments";
    }
}
```