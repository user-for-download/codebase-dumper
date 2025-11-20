# Codebase Dumper (Rust)

A high-performance CLI tool written in Rust to scan, clean, and dump source code into chunked text files. Designed for preparing codebases for LLM context (ChatGPT, Claude, etc.) or archiving.

## 🚀 Features

*   **Recursive Scanning:** fast directory traversal.
*   **Smart Cleaning:** Removes comments (`//`, `/* */`, `#`) and empty lines while preserving strings. automatically detects comment style based on file extension.
*   **Chunking:** Splits output into multiple files (e.g., `dump_1.txt`, `dump_2.txt`) based on a character limit. **Never crops files** (moves large files to the next chunk).
*   **Tree View:** Generates a visual directory tree at the top of the first output file.
*   **Filtering:** Strong Include/Exclude logic.
*   **External Includes:** Can include files outside the scanned directory (e.g., `.env` in the parent folder).
*   **Safety:** Automatically wipes the output directory before starting, but prevents accidental deletion of source code.

## 🛠 Installation

You need [Rust installed](https://www.rust-lang.org/tools/install).

```bash
# Build release binary
cargo build --release

# Move binary to current folder (optional)
cp target/release/search_tool .
```

## 📖 Usage

### Basic Syntax
```bash
./search_tool --path <SOURCE> --type <EXT> --out <OUTPUT_PATTERN> [OPTIONS]
```

### Arguments

| Flag | Description | Required |
| :--- | :--- | :--- |
| `--path` | Path to the source directory to scan. | Yes |
| `--type` | Main file extension to filter (e.g., `.php`, `.rs`, `.py`). | Yes |
| `--out` | Output pattern. Must include `*` for numbering. <br>Example: `"dump/file_*.txt"` | Yes |
| `--clean` | Removes comments and empty lines to save space. | No |
| `--include`| Comma-separated whitelist. Can be filenames, paths, or substrings. <br>Example: `--include .env,composer.json` | No |
| `--exclude`| Comma-separated blacklist. Skips any path containing these strings.<br>Example: `--exclude vendor,node_modules` | No |
| `--limit` | Character limit per output file (Default: 100,000). | No |
| `--progress`| Shows a progress bar. | No |

## 💡 Examples

### 1. PHP Project (with .env file)
Scans `site03/app` for PHP files, but *also* includes the `.env` file from the parent folder, and dumps everything to `site03/dump/`.

```bash
./search_tool \
  --path site03/app \
  --type .php \
  --clean \
  --out "site03/dump/dump_*.txt" \
  --include site03/.env,composer.json \
  --exclude tests,Console \
  --progress
```

### 2. Rust Project
Scans current directory, excludes `target` and `.git`.

```bash
./search_tool \
  --path . \
  --type .rs \
  --clean \
  --out "dump/rust_code_*.txt" \
  --exclude target,.git
```

### 3. Python Project (Script style comments)
The tool automatically detects `.py` files and uses `#` for comment removal instead of `//`.

```bash
./search_tool \
  --path ./my_script \
  --type .py \
  --clean \
  --out "output/script_*.txt"
```

## ⚠️ Safety & Validation

1.  **Output Wiping:** The tool will **DELETE** the folder specified in `--out` to ensure a clean dump.
    *   *Safety:* It checks if your Source Path is inside the Output Path. If it is, it refuses to delete to protect your code.
2.  **Validation:** At the end of the run, it warns you if an `--include` pattern was not found or if an `--exclude` pattern was never triggered.

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
class User { ... }
