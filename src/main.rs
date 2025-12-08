use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use once_cell::sync::Lazy;
use regex::{Captures, Regex, RegexBuilder};
use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

// ============================================================================
// CONSTANTS
// ============================================================================

/// Maximum file size to process (50MB)
const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;

/// Maximum regex size limit (10MB)
const REGEX_SIZE_LIMIT: usize = 10 * (1 << 20);

/// Maximum tree depth cap for safety
const MAX_TREE_DEPTH_CAP: usize = 100;

// ============================================================================
// LAZY STATIC REGEX PATTERNS
// ============================================================================

static C_STYLE_REGEX: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(
        r#"(?P<keep>`[^`\\]*(?:\\.[^`\\]*)*`|"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>/\*[\s\S]*?\*/|//.*)"#,
    )
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .expect("Invalid C-style regex")
});

static SCRIPT_STYLE_REGEX: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(
        r#"(?P<keep>"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>#.*)"#,
    )
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .expect("Invalid script-style regex")
});

static PHP_STYLE_REGEX: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(
        r#"(?P<keep>`[^`\\]*(?:\\.[^`\\]*)*`|"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>/\*[\s\S]*?\*/|//.*|#.*)"#,
    )
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .expect("Invalid PHP-style regex")
});

static HTML_STYLE_REGEX: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(
        r#"(?P<keep>"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop><!--[\s\S]*?-->)"#,
    )
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .expect("Invalid HTML-style regex")
});

static SQL_STYLE_REGEX: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(
        r#"(?P<keep>"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>/\*[\s\S]*?\*/|--.*)"#,
    )
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .expect("Invalid SQL-style regex")
});

static EMPTY_LINES_REGEX: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(r"(?m)(^\s*\n)+")
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .expect("Invalid empty lines regex")
});

static BRACE_REGEX: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(r"^(.*?)\{([^{}]+)}(.*)$")
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .expect("Invalid brace regex")
});

// ============================================================================
// DEFAULT CONFIG TEMPLATE
// ============================================================================

const DEFAULT_CONFIG: &str = r#"# ============================================================================
# Source Dumper Configuration (.dumperrc)
# ============================================================================
# Place this file in your project root directory.
# All settings here can be overridden by command-line arguments.
# Lines starting with # are comments.
# ============================================================================

# ----------------------------------------------------------------------------
# SOURCE SETTINGS
# ----------------------------------------------------------------------------

# Source directory path to scan (default: current directory)
# path = .

# Main file extension to process (required if not specified on command line)
# Examples: php, rs, js, ts, py, go, java, cpp
# type = php

# ----------------------------------------------------------------------------
# OUTPUT SETTINGS
# ----------------------------------------------------------------------------

# Output path pattern
# Placeholders:
#   * or {index} - chunk number (1, 2, 3, ...)
#   {type} - file extension without dot
# out = dump/dump_*.txt

# Output format: plain, markdown, json
# format = plain

# Character limit per output file (default: 110000)
# Adjust based on your LLM's context window
# limit = 110000

# Maximum file size to process in bytes (default: 52428800 = 50MB)
# max_file_size = 52428800

# ----------------------------------------------------------------------------
# FILTERING
# ----------------------------------------------------------------------------

# Exclude directories/files (comma-separated)
# Supports brace expansion: src/{tests,vendor}
exclude = vendor, node_modules, .git, .idea, .vscode, storage, cache, logs, tmp, temp, dist, build, coverage

# Include specific files even if they don't match the type
# Useful for config files, dotfiles, etc.
# include = .env.example, docker-compose.yml, Makefile

# ----------------------------------------------------------------------------
# PROCESSING OPTIONS
# ----------------------------------------------------------------------------

# Remove comments and empty lines from source files
# clean = false

# Show progress bar during processing
# progress = true

# Show verbose output (processing details)
# verbose = false

# Detect file type from shebang if extension is missing
# detect_shebang = true

# ----------------------------------------------------------------------------
# TREE VIEW OPTIONS
# ----------------------------------------------------------------------------

# Skip tree view generation in output
# no_tree = false

# Maximum tree depth to display (empty = unlimited, max 100)
# tree_depth = 20

# Show file sizes in tree view
# show_size = false

# Include hidden files (starting with .)
# hidden = false
"#;

// ============================================================================
// OUTPUT FORMAT ENUM
// ============================================================================

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Plain text output
    #[default]
    Plain,
    /// Markdown formatted output
    Markdown,
    /// JSON formatted output
    Json,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputFormat::Plain => write!(f, "plain"),
            OutputFormat::Markdown => write!(f, "markdown"),
            OutputFormat::Json => write!(f, "json"),
        }
    }
}

// ============================================================================
// CLI ARGUMENTS
// ============================================================================

#[derive(Parser, Debug, Clone)]
#[command(
    name = "source-dumper",
    author,
    version,
    about = "Aggregate source files into chunks for LLM context",
    long_about = None
)]
struct Args {
    /// Subcommand (init, config, run)
    #[command(subcommand)]
    command: Option<Commands>,

    /// Source directory path to search
    #[arg(long, default_value = ".")]
    path: PathBuf,

    /// Main file extension to filter (e.g., php, rs, js)
    #[arg(long = "type", value_name = "EXTENSION")]
    file_type: Option<String>,

    /// Clean content (remove comments and empty lines)
    #[arg(long)]
    clean: bool,

    /// Output path pattern (e.g. "dump/dump_*.txt")
    #[arg(long, default_value = "dump/dump_*.txt")]
    out: String,

    /// Output format
    #[arg(long, value_enum, default_value_t = OutputFormat::Plain)]
    format: OutputFormat,

    /// Show progress bar
    #[arg(long)]
    progress: bool,

    /// Verbose output
    #[arg(long, short)]
    verbose: bool,

    /// Dry run - show what would be processed
    #[arg(long)]
    dry_run: bool,

    /// Character limit per output file
    #[arg(long, default_value_t = 110000)]
    limit: usize,

    /// Maximum file size to process (in bytes)
    #[arg(long, default_value_t = MAX_FILE_SIZE)]
    max_file_size: u64,

    /// Exclude paths/folders
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    exclude: Vec<String>,

    /// Read exclude patterns from file(s)
    #[arg(long)]
    exclude_file: Vec<PathBuf>,

    /// Include specific files
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    include: Vec<String>,

    /// Read include patterns from file(s)
    #[arg(long)]
    include_file: Vec<PathBuf>,

    /// Config file path
    #[arg(long)]
    config: Option<PathBuf>,

    /// Maximum tree depth (empty for unlimited, max 100)
    #[arg(long)]
    tree_depth: Option<usize>,

    /// Skip tree view
    #[arg(long)]
    no_tree: bool,

    /// Include hidden files
    #[arg(long)]
    hidden: bool,

    /// Show file sizes in tree
    #[arg(long)]
    show_size: bool,

    /// Ignore .dumperrc config file
    #[arg(long)]
    no_config: bool,

    /// Detect file type from shebang
    #[arg(long, default_value_t = true)]
    detect_shebang: bool,
}

impl Args {
    /// Get effective tree depth, capped at MAX_TREE_DEPTH_CAP
    fn effective_tree_depth(&self) -> usize {
        self.tree_depth
            .map(|d| d.min(MAX_TREE_DEPTH_CAP))
            .unwrap_or(MAX_TREE_DEPTH_CAP)
    }
}

#[derive(Subcommand, Debug, Clone)]
enum Commands {
    /// Initialize a new .dumperrc config file
    Init {
        /// Force overwrite existing config
        #[arg(long, short)]
        force: bool,

        /// Output path for config file
        #[arg(long, default_value = ".dumperrc")]
        output: PathBuf,
    },

    /// Show current configuration
    Config {
        /// Show only non-default values
        #[arg(long)]
        diff: bool,
    },

    /// Run the dumper (default action)
    Run,
}

// ============================================================================
// STATISTICS
// ============================================================================

#[derive(Default, Debug, Clone)]
struct TreeStats {
    directories: usize,
    files: usize,
    total_size: u64,
}

impl TreeStats {
    fn summary(&self) -> String {
        format!(
            "\n{} directories, {} files{}",
            self.directories,
            self.files,
            if self.total_size > 0 {
                format!(", {}", format_size(self.total_size))
            } else {
                String::new()
            }
        )
    }
}

#[derive(Default, Debug, Clone)]
struct ProcessingStats {
    files_processed: usize,
    files_skipped: usize,
    files_too_large: usize,
    total_input_bytes: u64,
    total_output_bytes: u64,
    chunks_created: usize,
}

impl ProcessingStats {
    fn summary(&self) -> String {
        let compression = if self.total_input_bytes > 0 {
            let ratio = (self
                .total_input_bytes
                .saturating_sub(self.total_output_bytes)) as f64
                / self.total_input_bytes as f64
                * 100.0;
            format!(" ({:.1}% reduction)", ratio)
        } else {
            String::new()
        };

        let large_warning = if self.files_too_large > 0 {
            format!(" | Too large: {}", self.files_too_large)
        } else {
            String::new()
        };

        format!(
            "Processed: {} files | Skipped: {}{} | Input: {} | Output: {}{} | Chunks: {}",
            self.files_processed,
            self.files_skipped,
            large_warning,
            format_size(self.total_input_bytes),
            format_size(self.total_output_bytes),
            compression,
            self.chunks_created
        )
    }
}

// ============================================================================
// FILE INFO FOR JSON OUTPUT
// ============================================================================

#[derive(Debug, Clone)]
struct FileEntry {
    path: String,
    content: String,
    size: u64,
}

// ============================================================================
// MAIN
// ============================================================================

fn main() -> Result<()> {
    let mut args = Args::parse();

    // Handle subcommands
    match &args.command {
        Some(Commands::Init { force, output }) => {
            return cmd_init(*force, output);
        }
        Some(Commands::Config { diff }) => {
            return cmd_config(&args, *diff);
        }
        Some(Commands::Run) | None => {
            // Continue with main processing
        }
    }

    // Load config file unless --no-config
    if !args.no_config {
        load_config_file(&mut args)?;
    }

    // Validate required arguments
    let target_ext = match &args.file_type {
        Some(t) => t.trim_start_matches('.').to_lowercase(),
        None => {
            eprintln!("❌ Error: --type is required (e.g., --type php)");
            eprintln!("   Or set 'type = php' in .dumperrc");
            eprintln!("\n   Run 'source-dumper init' to create a config file.");
            std::process::exit(1);
        }
    };

    // Canonicalize source path
    if let Ok(p) = fs::canonicalize(&args.path) {
        args.path = p;
    }

    // Load patterns from files
    if !args.include_file.is_empty() {
        let patterns = load_patterns_from_files(&args.include_file)?;
        if args.verbose {
            println!("📄 Loaded {} include patterns from files", patterns.len());
        }
        args.include.extend(patterns);
    }

    if !args.exclude_file.is_empty() {
        let patterns = load_patterns_from_files(&args.exclude_file)?;
        if args.verbose {
            println!("📄 Loaded {} exclude patterns from files", patterns.len());
        }
        args.exclude.extend(patterns);
    }

    // Expand brace patterns
    args.include = expand_brace_patterns(args.include);
    args.exclude = expand_brace_patterns(args.exclude);

    if args.verbose {
        println!("🔧 Configuration:");
        println!("   Source: {:?}", args.path);
        println!("   Type: .{}", target_ext);
        println!("   Format: {}", args.format);
        println!("   Excludes: {:?}", args.exclude);
        println!("   Includes: {:?}", args.include);
        println!("   Limit: {} chars/file", args.limit);
        println!("   Max file size: {}", format_size(args.max_file_size));
        println!("   Clean: {}", args.clean);
        println!("   Dry Run: {}", args.dry_run);
        println!(
            "   Tree Depth: {}",
            args.tree_depth
                .map(|d| d.to_string())
                .unwrap_or_else(|| "unlimited".to_string())
        );
    }

    // Prepare output directory
    if !args.dry_run {
        prepare_output_directory(&args)?;
    }

    let display_ext = format!(".{}", target_ext);

    println!(
        "🔍 Scanning: {:?} | Type: {} | Format: {} | Includes: {}",
        args.path,
        display_ext,
        args.format,
        args.include.len()
    );

    // Collect files
    let (files_to_process, matched_includes) = collect_files(&args, &target_ext)?;

    // Report unmatched includes
    if !args.include.is_empty() {
        let missing: Vec<_> = args
            .include
            .iter()
            .filter(|inc| !matched_includes.contains(*inc))
            .collect();

        if !missing.is_empty() {
            println!(
                "⚠️  WARNING: {} include patterns not matched:",
                missing.len()
            );
            for m in &missing {
                println!("   - {}", m);
            }
        }
    }

    let total_files = files_to_process.len() as u64;
    println!("📁 Found {} files to process.", total_files);

    if total_files == 0 {
        println!("Nothing to do.");
        return Ok(());
    }

    // Dry run mode
    if args.dry_run {
        println!("\n🔍 DRY RUN - Files that would be processed:");
        for (i, file) in files_to_process.iter().enumerate() {
            let size = fs::metadata(file).map(|m| m.len()).unwrap_or(0);
            let size_warning = if size > args.max_file_size {
                " ⚠️ TOO LARGE"
            } else {
                ""
            };
            println!(
                "   {}. {:?} ({}){}",
                i + 1,
                file,
                format_size(size),
                size_warning
            );
        }
        println!("\n✅ Dry run complete. No files were written.");
        return Ok(());
    }

    // Process files based on format
    match args.format {
        OutputFormat::Json => process_files_json(&args, &files_to_process, &target_ext)?,
        OutputFormat::Plain | OutputFormat::Markdown => {
            process_files_text(&args, &files_to_process, &target_ext)?
        }
    }

    Ok(())
}

// ============================================================================
// TEXT FORMAT PROCESSING (Plain and Markdown)
// ============================================================================

fn process_files_text(args: &Args, files_to_process: &[PathBuf], target_ext: &str) -> Result<()> {
    let mut stats = ProcessingStats::default();
    let total_files = files_to_process.len() as u64;
    let pb = create_progress_bar(args, total_files)?;

    let mut current_buffer = String::with_capacity(args.limit);
    let mut file_part_index = 1;

    // Generate tree view
    if !args.no_tree {
        let tree_content = generate_full_tree(args)?;
        current_buffer.push_str(&tree_content);
    }

    // Process each file
    for file_path in files_to_process {
        let result = process_single_file(
            file_path,
            args,
            target_ext,
            &mut current_buffer,
            &mut file_part_index,
            &mut stats,
            pb.as_ref(),
        );

        if let Err(e) = result {
            if args.verbose {
                log_with_progress(pb.as_ref(), &format!("⚠️  Error processing {:?}: {}", file_path, e));
            }
            stats.files_skipped += 1;
        }

        if let Some(ref pb) = pb {
            pb.inc(1);
        }
    }

    // Write remaining buffer
    if !current_buffer.is_empty() {
        let bytes_written = write_to_disk(&args.out, target_ext, file_part_index, &current_buffer)?;
        stats.total_output_bytes += bytes_written as u64;
        stats.chunks_created = file_part_index;
    }

    if let Some(ref pb) = pb {
        pb.finish_with_message("Done");
    }

    println!("\n✅ {}", stats.summary());

    Ok(())
}

// ============================================================================
// JSON FORMAT PROCESSING
// ============================================================================

fn process_files_json(args: &Args, files_to_process: &[PathBuf], target_ext: &str) -> Result<()> {
    let mut stats = ProcessingStats::default();
    let total_files = files_to_process.len() as u64;
    let pb = create_progress_bar(args, total_files)?;

    let mut all_entries: Vec<FileEntry> = Vec::new();
    let mut current_size = 0usize;
    let mut file_part_index = 1;

    // Add tree as first entry if enabled
    if !args.no_tree {
        let tree_content = generate_full_tree(args)?;
        all_entries.push(FileEntry {
            path: "_tree_".to_string(),
            content: tree_content.clone(),
            size: 0,
        });
        current_size += tree_content.len();
    }

    for file_path in files_to_process {
        // Check file size first
        let metadata = match fs::metadata(file_path) {
            Ok(m) => m,
            Err(e) => {
                if args.verbose {
                    log_with_progress(
                        pb.as_ref(),
                        &format!("⚠️  Cannot read metadata: {:?}: {}", file_path, e),
                    );
                }
                stats.files_skipped += 1;
                if let Some(ref pb) = pb {
                    pb.inc(1);
                }
                continue;
            }
        };

        if metadata.len() > args.max_file_size {
            if args.verbose {
                log_with_progress(
                    pb.as_ref(),
                    &format!(
                        "⚠️  Skipping large file: {:?} ({})",
                        file_path,
                        format_size(metadata.len())
                    ),
                );
            }
            stats.files_too_large += 1;
            if let Some(ref pb) = pb {
                pb.inc(1);
            }
            continue;
        }

        let content = match fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(e) => {
                if args.verbose {
                    log_with_progress(
                        pb.as_ref(),
                        &format!("⚠️  Cannot read file: {:?}: {}", file_path, e),
                    );
                }
                stats.files_skipped += 1;
                if let Some(ref pb) = pb {
                    pb.inc(1);
                }
                continue;
            }
        };

        stats.total_input_bytes += content.len() as u64;

        let processed_content = if args.clean {
            clean_content(file_path, &content)
        } else {
            content
        };

        if processed_content.is_empty() {
            stats.files_skipped += 1;
            if let Some(ref pb) = pb {
                pb.inc(1);
            }
            continue;
        }

        let relative_path = file_path
            .strip_prefix(&args.path)
            .unwrap_or(file_path)
            .display()
            .to_string();

        let entry_size = relative_path.len() + processed_content.len() + 50; // overhead for JSON

        // Check if we need to write current chunk
        if current_size + entry_size > args.limit && !all_entries.is_empty() {
            let bytes_written =
                write_json_to_disk(&args.out, target_ext, file_part_index, &all_entries)?;
            stats.total_output_bytes += bytes_written as u64;
            stats.chunks_created = file_part_index;
            all_entries.clear();
            current_size = 0;
            file_part_index += 1;
        }

        all_entries.push(FileEntry {
            path: relative_path,
            content: processed_content.clone(),
            size: metadata.len(),
        });
        current_size += entry_size;
        stats.files_processed += 1;

        if let Some(ref pb) = pb {
            pb.inc(1);
        }
    }

    // Write remaining entries
    if !all_entries.is_empty() {
        let bytes_written =
            write_json_to_disk(&args.out, target_ext, file_part_index, &all_entries)?;
        stats.total_output_bytes += bytes_written as u64;
        stats.chunks_created = file_part_index;
    }

    if let Some(ref pb) = pb {
        pb.finish_with_message("Done");
    }

    println!("\n✅ {}", stats.summary());

    Ok(())
}

fn write_json_to_disk(
    out_pattern: &str,
    ext: &str,
    index: usize,
    entries: &[FileEntry],
) -> Result<usize> {
    let filename = generate_output_filename(out_pattern, ext, index);
    let path = PathBuf::from(&filename);

    if let Some(parent) = path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }

    // Build JSON manually to avoid serde dependency
    let mut json = String::from("{\n  \"files\": [\n");

    for (i, entry) in entries.iter().enumerate() {
        let escaped_content = escape_json_string(&entry.content);
        let escaped_path = escape_json_string(&entry.path);

        json.push_str(&format!(
            "    {{\n      \"path\": \"{}\",\n      \"size\": {},\n      \"content\": \"{}\"\n    }}",
            escaped_path,
            entry.size,
            escaped_content
        ));

        if i < entries.len() - 1 {
            json.push(',');
        }
        json.push('\n');
    }

    json.push_str("  ]\n}");

    let mut file =
        File::create(&path).context(format!("Failed to create output file: {:?}", path))?;

    let bytes = json.as_bytes();
    file.write_all(bytes)?;

    println!("💾 Saved: {:?} ({})", path, format_size(bytes.len() as u64));

    Ok(bytes.len())
}

fn escape_json_string(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c.is_control() => {
                result.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => result.push(c),
        }
    }
    result
}

// ============================================================================
// SUBCOMMAND: INIT
// ============================================================================

fn cmd_init(force: bool, output: &PathBuf) -> Result<()> {
    println!("📝 Initializing configuration file...\n");

    if output.exists() && !force {
        println!("⚠️  Config file already exists: {:?}", output);
        println!("   Use --force to overwrite.");
        return Ok(());
    }

    let mut file =
        File::create(output).context(format!("Failed to create config file: {:?}", output))?;

    file.write_all(DEFAULT_CONFIG.as_bytes())?;

    println!("✅ Created: {:?}", output);
    println!();
    println!("📖 Quick Start:");
    println!("   1. Edit .dumperrc and set 'type = <your-extension>'");
    println!("   2. Customize exclude patterns as needed");
    println!("   3. Run: source-dumper");
    println!();
    println!("📚 Examples:");
    println!("   source-dumper --type php");
    println!("   source-dumper --type rs --clean --progress");
    println!("   source-dumper --type js --format markdown");
    println!("   source-dumper --dry-run --verbose");

    Ok(())
}

// ============================================================================
// SUBCOMMAND: CONFIG
// ============================================================================

fn cmd_config(args: &Args, diff_only: bool) -> Result<()> {
    let mut display_args = args.clone();

    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from(".dumperrc"));
    let config_exists = config_path.exists();

    if config_exists {
        load_config_file(&mut display_args)?;
        println!("📄 Config file: {:?}", config_path);
    } else {
        println!("📄 Config file: (none found)");
    }

    println!();
    println!("🔧 Current Configuration:");
    println!("─────────────────────────────────────────");

    let show = |key: &str, value: &str, is_default: bool| {
        if diff_only && is_default {
            return;
        }
        let marker = if is_default { "(default)" } else { "←" };
        println!("   {:15} = {:30} {}", key, value, marker);
    };

    show(
        "path",
        &display_args.path.to_string_lossy(),
        display_args.path == PathBuf::from("."),
    );
    show(
        "type",
        display_args.file_type.as_deref().unwrap_or("(not set)"),
        display_args.file_type.is_none(),
    );
    show(
        "out",
        &display_args.out,
        display_args.out == "dump/dump_*.txt",
    );
    show(
        "format",
        &display_args.format.to_string(),
        display_args.format == OutputFormat::Plain,
    );
    show(
        "limit",
        &display_args.limit.to_string(),
        display_args.limit == 110000,
    );
    show(
        "max_file_size",
        &format_size(display_args.max_file_size),
        display_args.max_file_size == MAX_FILE_SIZE,
    );
    show(
        "clean",
        &display_args.clean.to_string(),
        !display_args.clean,
    );
    show(
        "progress",
        &display_args.progress.to_string(),
        !display_args.progress,
    );
    show(
        "verbose",
        &display_args.verbose.to_string(),
        !display_args.verbose,
    );
    show(
        "no_tree",
        &display_args.no_tree.to_string(),
        !display_args.no_tree,
    );
    show(
        "tree_depth",
        &display_args
            .tree_depth
            .map(|d| d.to_string())
            .unwrap_or_else(|| "unlimited".to_string()),
        display_args.tree_depth.is_none(),
    );
    show(
        "hidden",
        &display_args.hidden.to_string(),
        !display_args.hidden,
    );
    show(
        "show_size",
        &display_args.show_size.to_string(),
        !display_args.show_size,
    );
    show(
        "detect_shebang",
        &display_args.detect_shebang.to_string(),
        display_args.detect_shebang,
    );

    println!();

    if !display_args.exclude.is_empty() {
        println!("   Excludes ({}):", display_args.exclude.len());
        for ex in &display_args.exclude {
            println!("      - {}", ex);
        }
    }

    if !display_args.include.is_empty() {
        println!("   Includes ({}):", display_args.include.len());
        for inc in &display_args.include {
            println!("      - {}", inc);
        }
    }

    println!("─────────────────────────────────────────");

    Ok(())
}

// ============================================================================
// CONFIG FILE LOADING
// ============================================================================

fn load_config_file(args: &mut Args) -> Result<()> {
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from(".dumperrc"));

    if !config_path.exists() {
        return Ok(());
    }

    let file = File::open(&config_path)
        .context(format!("Failed to open config file: {:?}", config_path))?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"').trim_matches('\'');

            match key {
                "type" => {
                    if args.file_type.is_none() {
                        args.file_type = Some(value.to_string());
                    }
                }
                "path" => {
                    if args.path == PathBuf::from(".") {
                        args.path = PathBuf::from(value);
                    }
                }
                "out" => {
                    if args.out == "dump/dump_*.txt" {
                        args.out = value.to_string();
                    }
                }
                "format" => {
                    if args.format == OutputFormat::Plain {
                        args.format = match value.to_lowercase().as_str() {
                            "markdown" | "md" => OutputFormat::Markdown,
                            "json" => OutputFormat::Json,
                            _ => OutputFormat::Plain,
                        };
                    }
                }
                "limit" => {
                    if args.limit == 110000 {
                        if let Ok(limit) = value.parse() {
                            args.limit = limit;
                        }
                    }
                }
                "max_file_size" => {
                    if args.max_file_size == MAX_FILE_SIZE {
                        if let Ok(size) = value.parse() {
                            args.max_file_size = size;
                        }
                    }
                }
                "exclude" => {
                    args.exclude.extend(
                        value
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty()),
                    );
                }
                "include" => {
                    args.include.extend(
                        value
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty()),
                    );
                }
                "clean" => {
                    if !args.clean && (value == "true" || value == "1") {
                        args.clean = true;
                    }
                }
                "progress" => {
                    if !args.progress && (value == "true" || value == "1") {
                        args.progress = true;
                    }
                }
                "verbose" => {
                    if !args.verbose && (value == "true" || value == "1") {
                        args.verbose = true;
                    }
                }
                "hidden" => {
                    if !args.hidden && (value == "true" || value == "1") {
                        args.hidden = true;
                    }
                }
                "no_tree" => {
                    if !args.no_tree && (value == "true" || value == "1") {
                        args.no_tree = true;
                    }
                }
                "show_size" => {
                    if !args.show_size && (value == "true" || value == "1") {
                        args.show_size = true;
                    }
                }
                "detect_shebang" => {
                    if value == "false" || value == "0" {
                        args.detect_shebang = false;
                    }
                }
                "tree_depth" => {
                    if args.tree_depth.is_none() {
                        if let Ok(depth) = value.parse() {
                            args.tree_depth = Some(depth);
                        }
                    }
                }
                _ => {} // Unknown key
            }
        }
    }

    Ok(())
}

// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

/// Log a message, suspending the progress bar if it exists
fn log_with_progress(pb: Option<&ProgressBar>, message: &str) {
    if let Some(pb) = pb {
        pb.suspend(|| println!("{}", message));
    } else {
        println!("{}", message);
    }
}

/// Check if it's safe to delete the output directory
fn is_safe_to_delete(output_dir: &Path, source_dir: &Path) -> bool {
    let out = match fs::canonicalize(output_dir) {
        Ok(p) => p,
        Err(_) => return false, // Conservative: don't delete if we can't verify
    };

    let src = match fs::canonicalize(source_dir) {
        Ok(p) => p,
        Err(_) => return false,
    };

    // Check both directions to prevent any overlap
    !out.starts_with(&src) && !src.starts_with(&out) && out != src
}

fn prepare_output_directory(args: &Args) -> Result<()> {
    let out_path_obj = Path::new(&args.out);

    if let Some(parent_dir) = out_path_obj.parent() {
        if parent_dir.exists() && parent_dir != Path::new("") && parent_dir != Path::new(".") {
            if is_safe_to_delete(parent_dir, &args.path) {
                if args.verbose {
                    println!("🗑️  Wiping output directory: {:?}", parent_dir);
                }
                fs::remove_dir_all(parent_dir).context("Failed to delete output directory")?;
                fs::create_dir_all(parent_dir).context("Failed to recreate output directory")?;
            } else {
                println!("⚠️  Skipping deletion: Output folder overlaps with source.");
            }
        } else if !parent_dir.as_os_str().is_empty() && !parent_dir.exists() {
            fs::create_dir_all(parent_dir)?;
        }
    }

    Ok(())
}

fn collect_files(args: &Args, target_ext: &str) -> Result<(Vec<PathBuf>, HashSet<String>)> {
    let mut files_to_process = Vec::new();
    let mut matched_includes: HashSet<String> = HashSet::new();

    let walker = WalkDir::new(&args.path)
        .follow_links(true)
        .into_iter()
        .filter_entry(|e| !is_excluded_entry(e, &args.exclude, args.hidden));

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        let path_str = path.to_string_lossy();
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();

        let mut should_process = false;

        // Check by extension
        if let Some(ext) = path.extension() {
            if ext.to_string_lossy().to_lowercase() == target_ext {
                should_process = true;
            }
        }

        // Check by shebang if enabled and no extension match
        if !should_process && args.detect_shebang {
            if let Some(detected_type) = detect_file_type_from_shebang(path) {
                if detected_type == target_ext {
                    should_process = true;
                    if args.verbose {
                        println!(
                            "   🔍 Detected {} from shebang: {:?}",
                            detected_type, path
                        );
                    }
                }
            }
        }

        // Check include patterns
        for inc in &args.include {
            if matches_include_pattern(&file_name, &path_str, inc) {
                matched_includes.insert(inc.clone());
                should_process = true;
            }
        }

        if should_process {
            files_to_process.push(path.to_path_buf());

            if args.verbose {
                println!("   ✓ {:?}", path);
            }
        }
    }

    // External includes
    for inc in &args.include {
        let inc_path = Path::new(inc);

        if inc_path.exists() && inc_path.is_file() {
            let abs_inc = fs::canonicalize(inc_path).unwrap_or_else(|_| inc_path.to_path_buf());

            let already_exists = files_to_process
                .iter()
                .any(|p| fs::canonicalize(p).map(|cp| cp == abs_inc).unwrap_or(false));

            if !already_exists {
                files_to_process.push(inc_path.to_path_buf());
                matched_includes.insert(inc.clone());
                println!("   ➕ Added external include: {:?}", inc_path);
            } else {
                matched_includes.insert(inc.clone());
            }
        }
    }

    files_to_process.sort();

    Ok((files_to_process, matched_includes))
}

/// Detect file type from shebang line
fn detect_file_type_from_shebang(path: &Path) -> Option<&'static str> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();

    reader.read_line(&mut first_line).ok()?;

    if !first_line.starts_with("#!") {
        return None;
    }

    let shebang = first_line.to_lowercase();

    // Match common interpreters
    if shebang.contains("python3") || shebang.contains("python") {
        Some("py")
    } else if shebang.contains("node") || shebang.contains("nodejs") {
        Some("js")
    } else if shebang.contains("ruby") {
        Some("rb")
    } else if shebang.contains("perl") {
        Some("pl")
    } else if shebang.contains("bash") || shebang.contains("/sh") {
        Some("sh")
    } else if shebang.contains("zsh") {
        Some("zsh")
    } else if shebang.contains("php") {
        Some("php")
    } else if shebang.contains("lua") {
        Some("lua")
    } else if shebang.contains("Rscript") {
        Some("r")
    } else {
        None
    }
}

fn matches_include_pattern(file_name: &Cow<str>, path_str: &Cow<str>, pattern: &str) -> bool {
    // Exact filename match
    if file_name.as_ref() == pattern {
        return true;
    }

    // Path suffix match
    if path_str.ends_with(pattern) {
        let prefix_len = path_str.len() - pattern.len();
        if prefix_len == 0 {
            return true;
        }

        let prefix_char = path_str.chars().nth(prefix_len - 1);
        if prefix_char == Some('/') || prefix_char == Some('\\') {
            return true;
        }
    }

    // Path contains pattern (for patterns with path separators)
    if pattern.contains('/') || pattern.contains('\\') {
        let normalized_pattern = pattern.replace('\\', "/");
        let normalized_path = path_str.replace('\\', "/");
        if normalized_path.contains(&normalized_pattern) {
            return true;
        }
    }

    false
}

fn is_excluded_entry(entry: &DirEntry, excludes: &[String], include_hidden: bool) -> bool {
    let path = entry.path();
    let name = entry.file_name().to_string_lossy();

    // Check hidden files
    if !include_hidden && entry.depth() > 0 && name.starts_with('.') {
        return true;
    }

    let path_str = path.to_string_lossy();

    for excl in excludes {
        // Exact component match
        if path.components().any(|c| c.as_os_str() == excl.as_str()) {
            return true;
        }

        // Path contains pattern
        if (excl.contains('/') || excl.contains('\\')) && path_str.contains(excl) {
            return true;
        }

        // Glob pattern
        if excl.contains('*') {
            let pattern = excl.replace('.', r"\.").replace('*', ".*");
            if let Ok(re) = Regex::new(&format!("(?i){}", pattern)) {
                if re.is_match(&name) {
                    return true;
                }
            }
        }
    }

    false
}

fn load_patterns_from_files(paths: &[PathBuf]) -> Result<Vec<String>> {
    let mut patterns = Vec::new();

    for path in paths {
        let file = File::open(path).context(format!("Failed to open pattern file: {:?}", path))?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();

            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                patterns.push(trimmed.to_string());
            }
        }
    }

    Ok(patterns)
}

/// Recursively expand brace patterns like "src/{a,b,c}" into ["src/a", "src/b", "src/c"]
fn expand_brace_patterns(patterns: Vec<String>) -> Vec<String> {
    let mut result: HashSet<String> = HashSet::new();

    for pattern in patterns {
        expand_single_pattern(&pattern, &mut result);
    }

    let mut sorted: Vec<String> = result.into_iter().collect();
    sorted.sort();
    sorted
}

fn expand_single_pattern(pattern: &str, result: &mut HashSet<String>) {
    if let Some(caps) = BRACE_REGEX.captures(pattern) {
        let prefix = &caps[1];
        let content = &caps[2];
        let suffix = &caps[3];

        for part in content.split(',') {
            let expanded = format!("{}{}{}", prefix, part.trim(), suffix);
            // Recursively expand in case of nested braces
            expand_single_pattern(&expanded, result);
        }
    } else {
        result.insert(pattern.to_string());
    }
}

fn create_progress_bar(args: &Args, total: u64) -> Result<Option<ProgressBar>> {
    if args.progress {
        let pb = ProgressBar::new(total);
        pb.set_style(
            ProgressStyle::default_bar()
                .template(
                    "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
                )?
                .progress_chars("#>-"),
        );
        Ok(Some(pb))
    } else {
        Ok(None)
    }
}

fn generate_full_tree(args: &Args) -> Result<String> {
    let mut output = String::new();
    let mut stats = TreeStats::default();
    let mut visited_dirs = HashSet::new();

    match args.format {
        OutputFormat::Markdown => {
            output.push_str(&format!("# Project Structure: {:?}\n\n", args.path));
            output.push_str("```\n");
        }
        _ => {
            output.push_str(&format!("PROJECT STRUCTURE: {:?}\n", args.path));
            output.push_str("==========================================\n");
        }
    }

    let root_name = args
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());
    output.push_str(&format!("{}/\n", root_name));

    let effective_depth = args.effective_tree_depth();

    output.push_str(&generate_tree_view(
        &args.path,
        &args.exclude,
        String::new(),
        0,
        effective_depth,
        args.hidden,
        args.show_size,
        &mut visited_dirs,
        &mut stats,
    ));

    output.push_str(&stats.summary());

    match args.format {
        OutputFormat::Markdown => {
            output.push_str("\n```\n\n");
        }
        _ => {
            output.push_str("\n==========================================\n\n");
        }
    }

    Ok(output)
}

fn generate_tree_view(
    dir: &Path,
    excludes: &[String],
    prefix: String,
    depth: usize,
    max_depth: usize,
    include_hidden: bool,
    show_size: bool,
    visited: &mut HashSet<PathBuf>,
    stats: &mut TreeStats,
) -> String {
    if depth >= max_depth {
        return format!("{}... (max depth reached)\n", prefix);
    }

    if let Ok(canonical) = fs::canonicalize(dir) {
        if !visited.insert(canonical) {
            return format!("{}... (symlink cycle)\n", prefix);
        }
    }

    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return String::new(),
    };

    let mut entries: Vec<_> = read_dir
        .filter_map(|e| e.ok())
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();

            if !include_hidden && name.starts_with('.') {
                return false;
            }

            if excludes.iter().any(|ex| name == *ex) {
                return false;
            }

            true
        })
        .collect();

    // Sort: directories first, then alphabetically
    entries.sort_by(|a, b| {
        let a_dir = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let b_dir = b.file_type().map(|t| t.is_dir()).unwrap_or(false);

        match (a_dir, b_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.file_name().cmp(&b.file_name()),
        }
    });

    let mut output = String::new();
    let total = entries.len();

    for (i, entry) in entries.into_iter().enumerate() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_last = i == total - 1;
        let is_dir = path.is_dir();

        let connector = if is_last { "└── " } else { "├── " };

        let size_info = if show_size && !is_dir {
            if let Ok(meta) = fs::metadata(&path) {
                format!(" ({})", format_size(meta.len()))
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        let dir_indicator = if is_dir { "/" } else { "" };

        output.push_str(&format!(
            "{}{}{}{}{}\n",
            prefix, connector, name, dir_indicator, size_info
        ));

        if is_dir {
            stats.directories += 1;

            let child_prefix = if is_last {
                format!("{}    ", prefix)
            } else {
                format!("{}│   ", prefix)
            };

            output.push_str(&generate_tree_view(
                &path,
                excludes,
                child_prefix,
                depth + 1,
                max_depth,
                include_hidden,
                show_size,
                visited,
                stats,
            ));
        } else {
            stats.files += 1;
            if let Ok(meta) = fs::metadata(&path) {
                stats.total_size += meta.len();
            }
        }
    }

    output
}

fn process_single_file(
    file_path: &Path,
    args: &Args,
    target_ext: &str,
    current_buffer: &mut String,
    file_part_index: &mut usize,
    stats: &mut ProcessingStats,
    pb: Option<&ProgressBar>,
) -> Result<()> {
    // Check file size first
    let metadata = fs::metadata(file_path).with_context(|| {
        format!("Failed to read metadata for file: {:?}", file_path)
    })?;

    if metadata.len() > args.max_file_size {
        if args.verbose {
            log_with_progress(
                pb,
                &format!(
                    "⚠️  Skipping large file: {:?} ({})",
                    file_path,
                    format_size(metadata.len())
                ),
            );
        }
        stats.files_too_large += 1;
        return Ok(());
    }

    let content = fs::read_to_string(file_path).with_context(|| {
        format!(
            "Failed to read file: {:?} (size: {})",
            file_path,
            format_size(metadata.len())
        )
    })?;

    stats.total_input_bytes += content.len() as u64;

    let processed_content = if args.clean {
        clean_content(file_path, &content)
    } else {
        content
    };

    if processed_content.is_empty() {
        stats.files_skipped += 1;
        return Ok(());
    }

    stats.files_processed += 1;

    let relative_path = file_path.strip_prefix(&args.path).unwrap_or(file_path);

    let header = match args.format {
        OutputFormat::Markdown => {
            format!(
                "\n## File: `{}`\n\n```{}\n",
                relative_path.display(),
                get_markdown_lang(file_path)
            )
        }
        _ => format!("\n--- FILE: {} ---\n", relative_path.display()),
    };

    let footer = match args.format {
        OutputFormat::Markdown => "\n```\n".to_string(),
        _ => String::new(),
    };

    let chunk_len = header.len() + processed_content.len() + footer.len() + 1;

    // Check if we need to write current buffer to disk
    if !current_buffer.is_empty() && (current_buffer.len() + chunk_len > args.limit) {
        let bytes_written = write_to_disk(&args.out, target_ext, *file_part_index, current_buffer)?;
        stats.total_output_bytes += bytes_written as u64;
        stats.chunks_created = *file_part_index;
        current_buffer.clear();
        *file_part_index += 1;
    }

    current_buffer.push_str(&header);
    current_buffer.push_str(&processed_content);
    current_buffer.push_str(&footer);
    current_buffer.push('\n');

    Ok(())
}

/// Get language identifier for Markdown code blocks
fn get_markdown_lang(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "rs" => "rust",
        "py" => "python",
        "js" => "javascript",
        "ts" => "typescript",
        "rb" => "ruby",
        "go" => "go",
        "java" => "java",
        "cpp" | "cc" | "cxx" => "cpp",
        "c" | "h" => "c",
        "cs" => "csharp",
        "php" => "php",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "scala" => "scala",
        "sh" | "bash" | "zsh" => "bash",
        "sql" => "sql",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" | "sass" => "scss",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "xml" => "xml",
        "md" | "markdown" => "markdown",
        "vue" => "vue",
        "svelte" => "svelte",
        "jsx" => "jsx",
        "tsx" => "tsx",
        _ => "",
    }
}

fn clean_content(file_path: &Path, content: &str) -> String {
    let ext = file_path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let file_name = file_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();

    let regex: &Regex = match ext.as_str() {
        "py" | "rb" | "pl" | "sh" | "bash" | "zsh" | "yaml" | "yml" | "env" | "toml" | "conf"
        | "ini" | "dockerfile" | "makefile" | "cmake" | "r" | "jl" => &SCRIPT_STYLE_REGEX,
        "php" | "php3" | "php4" | "php5" | "phtml" => &PHP_STYLE_REGEX,
        "html" | "htm" | "xml" | "xhtml" | "svg" | "vue" | "svelte" => &HTML_STYLE_REGEX,
        "sql" | "mysql" | "pgsql" | "sqlite" => &SQL_STYLE_REGEX,
        _ if file_name == "dockerfile"
            || file_name == "makefile"
            || file_name == "cmakelists.txt"
            || file_name == ".gitignore"
            || file_name == ".dockerignore"
            || file_name == ".env" =>
            {
                &SCRIPT_STYLE_REGEX
            }
        _ => &C_STYLE_REGEX,
    };

    let temp_text = regex.replace_all(content, |caps: &Captures| {
        if let Some(m) = caps.name("keep") {
            m.as_str().to_string()
        } else {
            String::new()
        }
    });

    EMPTY_LINES_REGEX
        .replace_all(&temp_text, "\n")
        .trim()
        .to_string()
}

fn generate_output_filename(out_pattern: &str, ext: &str, index: usize) -> String {
    let ext_no_dot = ext.trim_start_matches('.');

    let mut filename = out_pattern
        .replace("{type}", ext_no_dot)
        .replace("{ext}", ext_no_dot)
        .replace("{index}", &index.to_string());

    if filename.contains('*') {
        filename = filename.replace('*', &index.to_string());
    } else if !filename.contains(&index.to_string()) {
        let path_obj = Path::new(&filename);
        let parent = path_obj.parent().unwrap_or(Path::new("."));
        let stem = path_obj.file_stem().unwrap_or_default().to_string_lossy();
        let extension = path_obj
            .extension()
            .map(|e| e.to_string_lossy())
            .unwrap_or_default();

        let new_name = if extension.is_empty() {
            format!("{}_{}", stem, index)
        } else {
            format!("{}_{}.{}", stem, index, extension)
        };

        filename = parent.join(new_name).to_string_lossy().to_string();
    }

    filename
}

fn write_to_disk(out_pattern: &str, ext: &str, index: usize, content: &str) -> Result<usize> {
    let filename = generate_output_filename(out_pattern, ext, index);
    let path = PathBuf::from(&filename);

    if let Some(parent) = path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }

    let mut file =
        File::create(&path).context(format!("Failed to create output file: {:?}", path))?;

    let bytes = content.as_bytes();
    file.write_all(bytes)?;

    println!("💾 Saved: {:?} ({})", path, format_size(bytes.len() as u64));

    Ok(bytes.len())
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(1048576), "1.00 MB");
        assert_eq!(format_size(1073741824), "1.00 GB");
    }

    #[test]
    fn test_expand_brace_patterns() {
        let patterns = vec!["src/{a,b,c}".to_string()];
        let expanded = expand_brace_patterns(patterns);
        assert!(expanded.contains(&"src/a".to_string()));
        assert!(expanded.contains(&"src/b".to_string()));
        assert!(expanded.contains(&"src/c".to_string()));
    }

    #[test]
    fn test_expand_nested_brace_patterns() {
        let patterns = vec!["{a,{b,c}}".to_string()];
        let expanded = expand_brace_patterns(patterns);
        assert!(expanded.contains(&"a".to_string()));
        assert!(expanded.contains(&"b".to_string()));
        assert!(expanded.contains(&"c".to_string()));
    }

    #[test]
    fn test_escape_json_string() {
        assert_eq!(escape_json_string("hello"), "hello");
        assert_eq!(escape_json_string("hello\nworld"), "hello\\nworld");
        assert_eq!(escape_json_string("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(escape_json_string("path\\to\\file"), "path\\\\to\\\\file");
    }

    #[test]
    fn test_matches_include_pattern() {
        let file_name: Cow<str> = Cow::Borrowed("config.json");
        let path_str: Cow<str> = Cow::Borrowed("/home/user/project/config.json");

        assert!(matches_include_pattern(&file_name, &path_str, "config.json"));
        assert!(matches_include_pattern(
            &file_name,
            &path_str,
            "project/config.json"
        ));
        assert!(!matches_include_pattern(&file_name, &path_str, "other.json"));
    }

    #[test]
    fn test_get_markdown_lang() {
        assert_eq!(get_markdown_lang(Path::new("test.rs")), "rust");
        assert_eq!(get_markdown_lang(Path::new("test.py")), "python");
        assert_eq!(get_markdown_lang(Path::new("test.js")), "javascript");
        assert_eq!(get_markdown_lang(Path::new("test.unknown")), "");
    }

    #[test]
    fn test_clean_content_c_style() {
        let content = r#"
            // This is a comment
            let x = 5; /* inline comment */
            let y = "// not a comment";
        "#;

        let cleaned = clean_content(Path::new("test.rs"), content);
        assert!(!cleaned.contains("This is a comment"));
        assert!(!cleaned.contains("inline comment"));
        assert!(cleaned.contains("// not a comment")); // Inside string, preserved
    }

    #[test]
    fn test_generate_output_filename() {
        assert_eq!(
            generate_output_filename("dump/dump_*.txt", "rs", 1),
            "dump/dump_1.txt"
        );
        assert_eq!(
            generate_output_filename("out/{type}_{index}.txt", "php", 2),
            "out/php_2.txt"
        );
    }

    #[test]
    fn test_effective_tree_depth() {
        let mut args = Args::parse_from(["test", "--type", "rs"]);

        args.tree_depth = None;
        assert_eq!(args.effective_tree_depth(), MAX_TREE_DEPTH_CAP);

        args.tree_depth = Some(10);
        assert_eq!(args.effective_tree_depth(), 10);

        args.tree_depth = Some(200);
        assert_eq!(args.effective_tree_depth(), MAX_TREE_DEPTH_CAP);
    }
}