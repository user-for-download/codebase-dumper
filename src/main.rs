use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use once_cell::sync::Lazy;
use regex::{Captures, Regex};
use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

// ============================================================================
// LAZY STATIC REGEX PATTERNS
// ============================================================================

static C_STYLE_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?P<keep>`[^`\\]*(?:\\.[^`\\]*)*`|"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>/\*[\s\S]*?\*/|//.*)"#,
    ).expect("Invalid C-style regex")
});

static SCRIPT_STYLE_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?P<keep>"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>#.*)"#)
        .expect("Invalid script-style regex")
});

static PHP_STYLE_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?P<keep>`[^`\\]*(?:\\.[^`\\]*)*`|"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>/\*[\s\S]*?\*/|//.*|#.*)"#,
    ).expect("Invalid PHP-style regex")
});

static HTML_STYLE_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?P<keep>"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop><!--[\s\S]*?-->)"#,
    )
    .expect("Invalid HTML-style regex")
});

static SQL_STYLE_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?P<keep>"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>/\*[\s\S]*?\*/|--.*)"#,
    ).expect("Invalid SQL-style regex")
});

static EMPTY_LINES_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)(^\s*\n)+").expect("Invalid empty lines regex"));

static BRACE_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(.*?)\{([^{}]+)}(.*)$").expect("Invalid brace regex"));

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

# Character limit per output file (default: 110000)
# Adjust based on your LLM's context window
# limit = 110000

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

# ----------------------------------------------------------------------------
# TREE VIEW OPTIONS
# ----------------------------------------------------------------------------

# Skip tree view generation in output
# no_tree = false

# Maximum tree depth to display (0 = unlimited, max 20)
# tree_depth = 20

# Show file sizes in tree view
# show_size = false

# Include hidden files (starting with .)
# hidden = false
"#;

// ============================================================================
// CLI ARGUMENTS
// ============================================================================

#[derive(Parser, Debug)]
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

    /// Main file extension to filter (e.g., .php, .rs)
    #[arg(long, value_name = "EXTENSION")]
    type_: Option<String>,

    /// Clean content (remove comments and empty lines)
    #[arg(long)]
    clean: bool,

    /// Output path pattern (e.g. "dump/dump_*.txt")
    #[arg(long, default_value = "dump/dump_*.txt")]
    out: String,

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

    /// Maximum tree depth
    #[arg(long, default_value_t = 20)]
    tree_depth: usize,

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
}

#[derive(Subcommand, Debug)]
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

#[derive(Default, Debug)]
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

#[derive(Default, Debug)]
struct ProcessingStats {
    files_processed: usize,
    files_skipped: usize,
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

        format!(
            "Processed: {} files | Skipped: {} | Input: {} | Output: {}{} | Chunks: {}",
            self.files_processed,
            self.files_skipped,
            format_size(self.total_input_bytes),
            format_size(self.total_output_bytes),
            compression,
            self.chunks_created
        )
    }
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
    let target_ext = match &args.type_ {
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
        println!("   Excludes: {:?}", args.exclude);
        println!("   Includes: {:?}", args.include);
        println!("   Limit: {} chars/file", args.limit);
        println!("   Clean: {}", args.clean);
        println!("   Dry Run: {}", args.dry_run);
    }

    // Prepare output directory
    if !args.dry_run {
        prepare_output_directory(&args)?;
    }

    let display_ext = format!(".{}", target_ext);

    println!(
        "🔍 Scanning: {:?} | Type: {} | Includes: {}",
        args.path,
        display_ext,
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
            println!("   {}. {:?} ({})", i + 1, file, format_size(size));
        }
        println!("\n✅ Dry run complete. No files were written.");
        return Ok(());
    }

    // Process files
    let mut stats = ProcessingStats::default();
    let pb = create_progress_bar(&args, total_files)?;

    let mut current_buffer = String::with_capacity(args.limit);
    let mut file_part_index = 1;

    // Generate tree view
    if !args.no_tree {
        let tree_content = generate_full_tree(&args)?;
        current_buffer.push_str(&tree_content);
    }

    // Process each file
    for file_path in &files_to_process {
        let result = process_single_file(
            file_path,
            &args,
            &target_ext,
            &mut current_buffer,
            &mut file_part_index,
            &mut stats,
        );

        if let Err(e) = result {
            if args.verbose {
                eprintln!("⚠️  Error processing {:?}: {}", file_path, e);
            }
            stats.files_skipped += 1;
        }

        if let Some(pb) = &pb {
            pb.inc(1);
        }
    }

    // Write remaining buffer
    if !current_buffer.is_empty() {
        let bytes_written =
            write_to_disk(&args.out, &target_ext, file_part_index, &current_buffer)?;
        stats.total_output_bytes += bytes_written as u64;
        stats.chunks_created = file_part_index;
    }

    if let Some(pb) = &pb {
        pb.finish_with_message("Done");
    }

    println!("\n✅ {}", stats.summary());

    Ok(())
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

    // Write config file
    let mut file =
        File::create(output).context(format!("Failed to create config file: {:?}", output))?;

    file.write_all(DEFAULT_CONFIG.as_bytes())?;

    println!("✅ Created: {:?}", output);
    println!();
    println!("📖 Quick Start:");
    println!("   1. Edit .dumperrc and set 'type = <your-extension>'");
    println!("   2. Customize exclude patterns as needed");
    println!("   3. Run:search_tool");
    println!();
    println!("📚 Examples:");
    println!("  search_tool --type php");
    println!("  search_tool --type rs --clean --progress");
    println!("  search_tool --dry-run --verbose");

    Ok(())
}

// ============================================================================
// SUBCOMMAND: CONFIG
// ============================================================================

fn cmd_config(args: &Args, diff_only: bool) -> Result<()> {
    let mut display_args = args.clone();

    // Try to load config
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
        display_args.type_.as_deref().unwrap_or("(not set)"),
        display_args.type_.is_none(),
    );
    show(
        "out",
        &display_args.out,
        display_args.out == "dump/dump_*.txt",
    );
    show(
        "limit",
        &display_args.limit.to_string(),
        display_args.limit == 110000,
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
        &display_args.tree_depth.to_string(),
        display_args.tree_depth == 20,
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
                    if args.type_.is_none() {
                        args.type_ = Some(value.to_string());
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
                "limit" => {
                    if args.limit == 110000 {
                        if let Ok(limit) = value.parse() {
                            args.limit = limit;
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
                "tree_depth" => {
                    if args.tree_depth == 20 {
                        if let Ok(depth) = value.parse() {
                            args.tree_depth = depth;
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

fn prepare_output_directory(args: &Args) -> Result<()> {
    let out_path_obj = Path::new(&args.out);

    if let Some(parent_dir) = out_path_obj.parent() {
        if parent_dir.exists() && parent_dir != Path::new("") && parent_dir != Path::new(".") {
            let abs_out = fs::canonicalize(parent_dir).unwrap_or_else(|_| parent_dir.to_path_buf());
            let abs_src = fs::canonicalize(&args.path).unwrap_or_else(|_| args.path.clone());

            let is_safe = abs_out != abs_src
                && !abs_src.starts_with(&abs_out)
                && !abs_out.starts_with(&abs_src);

            if is_safe {
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

        if let Some(ext) = path.extension() {
            if ext.to_string_lossy().to_lowercase() == target_ext {
                should_process = true;
            }
        }

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

fn matches_include_pattern(file_name: &Cow<str>, path_str: &Cow<str>, pattern: &str) -> bool {
    if file_name.as_ref() == pattern {
        return true;
    }

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

    if !include_hidden && entry.depth() > 0 && name.starts_with('.') {
        return true;
    }

    let path_str = path.to_string_lossy();

    for excl in excludes {
        if path.components().any(|c| c.as_os_str() == excl.as_str()) {
            return true;
        }

        if (excl.contains('/') || excl.contains('\\')) && path_str.contains(excl) {
            return true;
        }

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

fn expand_brace_patterns(patterns: Vec<String>) -> Vec<String> {
    let mut processed = Vec::new();
    let mut queue = patterns;

    while let Some(pattern) = queue.pop() {
        if let Some(caps) = BRACE_REGEX.captures(&pattern) {
            let prefix = &caps[1];
            let content = &caps[2];
            let suffix = &caps[3];

            for part in content.split(',') {
                queue.push(format!("{}{}{}", prefix, part.trim(), suffix));
            }
        } else {
            processed.push(pattern);
        }
    }

    let unique_set: HashSet<String> = processed.into_iter().collect();
    let mut result: Vec<String> = unique_set.into_iter().collect();
    result.sort();
    result
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

    output.push_str(&format!("PROJECT STRUCTURE: {:?}\n", args.path));
    output.push_str("==========================================\n");

    let root_name = args
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());
    output.push_str(&format!("{}/\n", root_name));

    output.push_str(&generate_tree_view(
        &args.path,
        &args.exclude,
        String::new(),
        0,
        args.tree_depth,
        args.hidden,
        args.show_size,
        &mut visited_dirs,
        &mut stats,
    ));

    output.push_str(&stats.summary());
    output.push_str("\n==========================================\n\n");

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
    let effective_max = if max_depth == 0 { 20 } else { max_depth };
    if depth >= effective_max {
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
) -> Result<()> {
    let content =
        fs::read_to_string(file_path).context(format!("Failed to read file: {:?}", file_path))?;

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

    let header = format!("\n--- FILE: {} ---\n", relative_path.display());
    let chunk_len = header.len() + processed_content.len() + 1;

    if !current_buffer.is_empty() && (current_buffer.len() + chunk_len > args.limit) {
        let bytes_written = write_to_disk(&args.out, target_ext, *file_part_index, current_buffer)?;
        stats.total_output_bytes += bytes_written as u64;
        stats.chunks_created = *file_part_index;
        current_buffer.clear();
        *file_part_index += 1;
    }

    current_buffer.push_str(&header);
    current_buffer.push_str(&processed_content);
    current_buffer.push('\n');

    Ok(())
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

fn write_to_disk(out_pattern: &str, ext: &str, index: usize, content: &str) -> Result<usize> {
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
// IMPLEMENT CLONE FOR ARGS (needed for config command)
// ============================================================================

impl Clone for Args {
    fn clone(&self) -> Self {
        Self {
            command: None,
            path: self.path.clone(),
            type_: self.type_.clone(),
            clean: self.clean,
            out: self.out.clone(),
            progress: self.progress,
            verbose: self.verbose,
            dry_run: self.dry_run,
            limit: self.limit,
            exclude: self.exclude.clone(),
            exclude_file: self.exclude_file.clone(),
            include: self.include.clone(),
            include_file: self.include_file.clone(),
            config: self.config.clone(),
            tree_depth: self.tree_depth,
            no_tree: self.no_tree,
            hidden: self.hidden,
            show_size: self.show_size,
            no_config: self.no_config,
        }
    }
}
