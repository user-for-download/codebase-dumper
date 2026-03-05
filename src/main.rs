use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use once_cell::sync::Lazy;
use regex::{Captures, Regex, RegexBuilder};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

// ============================================================================
// CONSTANTS
// ============================================================================

const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;
const REGEX_SIZE_LIMIT: usize = 10 * (1 << 20);
const MAX_TREE_DEPTH_CAP: usize = 100;
const DEFAULT_LIMIT: usize = 110_000;

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

# Source directory path to scan (default: current directory)
# path = .

# Main file extension to process (empty = all text files)
# Examples: php, rs, js, ts, py, go, java, cpp
# type =

# Output path pattern (* or {index} = chunk number, {type} = extension)
# out = dump/dump_*.txt

# Character limit per output file
# limit = 110000

# Maximum file size to process in bytes (default: 50MB)
# max_file_size = 52428800

# Exclude directories/files (comma-separated, supports brace expansion)
exclude = vendor, node_modules, .git, .idea, .vscode, storage, cache, logs, tmp, temp, dist, build, coverage, target

# Include specific files even if they don't match the type
# include = .env.example, docker-compose.yml, Makefile

# Remove comments and empty lines from source files
# clean = false

# Show progress bar during processing
# progress = true

# Show verbose output
# verbose = false

# Skip tree view generation in output
# no_tree = false

# Maximum tree depth (empty = unlimited, max 100)
# tree_depth = 20

# Show file sizes in tree view
# show_size = false

# Include hidden files (starting with .)
# hidden = false
"#;

// ============================================================================
// CLI ARGUMENTS
// ============================================================================

#[derive(Parser, Debug, Clone)]
#[command(
    name = "source-dumper",
    author,
    version,
    about = "Aggregate source files into text chunks for LLM context"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Source directory path
    #[arg(long, default_value = ".")]
    path: PathBuf,

    /// File extension filter (empty = all text files)
    #[arg(long = "type", value_name = "EXT")]
    file_type: Option<String>,

    /// Clean content (remove comments and empty lines)
    #[arg(long)]
    clean: bool,

    /// Output path pattern
    #[arg(long, default_value = "dump/dump_*.txt")]
    out: String,

    /// Show progress bar
    #[arg(long)]
    progress: bool,

    /// Verbose output
    #[arg(long, short)]
    verbose: bool,

    /// Dry run
    #[arg(long)]
    dry_run: bool,

    /// Character limit per output file
    #[arg(long, default_value_t = DEFAULT_LIMIT)]
    limit: usize,

    /// Maximum file size in bytes
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

    /// Maximum tree depth (max 100)
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
}

impl Args {
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
        #[arg(long, short)]
        force: bool,
        #[arg(long, default_value = ".dumperrc")]
        output: PathBuf,
    },
    /// Show current configuration
    Config {
        #[arg(long)]
        diff: bool,
    },
    /// Run the dumper (default)
    Run,
}

// ============================================================================
// COLLECTED FILE
// ============================================================================

#[derive(Debug, Clone)]
struct CollectedFile {
    path: PathBuf,
    size: u64,
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
    files_too_large: usize,
    files_binary: usize,
    total_input_bytes: u64,
    total_output_bytes: u64,
    chunks_created: usize,
}

impl ProcessingStats {
    fn summary(&self) -> String {
        let compression = if self.total_input_bytes > 0 {
            let ratio = self
                .total_input_bytes
                .saturating_sub(self.total_output_bytes) as f64
                / self.total_input_bytes as f64
                * 100.0;
            format!(" ({:.1}% reduction)", ratio)
        } else {
            String::new()
        };

        let mut extras = Vec::new();
        if self.files_too_large > 0 {
            extras.push(format!("too large: {}", self.files_too_large));
        }
        if self.files_binary > 0 {
            extras.push(format!("binary: {}", self.files_binary));
        }
        let extra_str = if extras.is_empty() {
            String::new()
        } else {
            format!(" ({})", extras.join(", "))
        };

        format!(
            "Processed: {} files | Skipped: {}{} | Input: {} | Output: {}{} | Chunks: {}",
            self.files_processed,
            self.files_skipped,
            extra_str,
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

    match &args.command {
        Some(Commands::Init { force, output }) => return cmd_init(*force, output),
        Some(Commands::Config { diff }) => return cmd_config(&args, *diff),
        Some(Commands::Run) | None => {}
    }

    if !args.no_config {
        load_config_file(&mut args)?;
    }

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

    // Normalize target extension
    let target_ext: Option<String> = args
        .file_type
        .as_ref()
        .map(|t| t.trim_start_matches('.').to_lowercase())
        .filter(|t| !t.is_empty());

    let display_type = target_ext
        .as_ref()
        .map(|e| format!(".{}", e))
        .unwrap_or_else(|| "(all text files)".to_string());

    if args.verbose {
        println!("🔧 Configuration:");
        println!("   Source: {:?}", args.path);
        println!("   Type: {}", display_type);
        println!("   Excludes: {:?}", args.exclude);
        println!("   Includes: {:?}", args.include);
        println!("   Limit: {} chars/file", args.limit);
        println!("   Max file size: {}", format_size(args.max_file_size));
        println!("   Clean: {}", args.clean);
        println!("   Dry Run: {}", args.dry_run);
    }

    // Prepare output directory
    if !args.dry_run {
        prepare_output_directory(&args)?;
    }

    println!(
        "🔍 Scanning: {:?} | Type: {} | Includes: {}",
        args.path,
        display_type,
        args.include.len()
    );

    // Collect files
    let (files_to_process, matched_includes) = collect_files(&args, target_ext.as_deref())?;

    // Report unmatched includes
    if !args.include.is_empty() {
        let missing: Vec<_> = args
            .include
            .iter()
            .filter(|inc| !matched_includes.contains(*inc))
            .collect();
        if !missing.is_empty() {
            println!("⚠️  {} include patterns not matched:", missing.len());
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

    if args.dry_run {
        println!("\n🔍 DRY RUN — files that would be processed:");
        for (i, cf) in files_to_process.iter().enumerate() {
            let warning = if cf.size > args.max_file_size {
                " ⚠️ TOO LARGE"
            } else {
                ""
            };
            println!(
                "   {}. {:?} ({}){}",
                i + 1,
                cf.path,
                format_size(cf.size),
                warning
            );
        }
        println!("\n✅ Dry run complete. No files were written.");
        return Ok(());
    }

    process_files_text(&args, &files_to_process, target_ext.as_deref())?;

    Ok(())
}

// ============================================================================
// TEXT PROCESSING
// ============================================================================

fn process_files_text(
    args: &Args,
    files: &[CollectedFile],
    target_ext: Option<&str>,
) -> Result<()> {
    let mut stats = ProcessingStats::default();
    let total = files.len() as u64;
    let pb = create_progress_bar(args, total)?;

    let mut buffer = String::with_capacity(args.limit);
    let mut chunk_index = 1usize;
    let ext_label = target_ext.unwrap_or("all");

    // Tree view
    if !args.no_tree {
        let tree = generate_full_tree(args)?;
        buffer.push_str(&tree);
    }

    for cf in files {
        if cf.size > args.max_file_size {
            if args.verbose {
                log_pb(
                    pb.as_ref(),
                    &format!(
                        "⚠️  Skipping large file: {:?} ({})",
                        cf.path,
                        format_size(cf.size)
                    ),
                );
            }
            stats.files_too_large += 1;
            stats.files_skipped += 1;
            if let Some(ref pb) = pb {
                pb.inc(1);
            }
            continue;
        }

        // Binary check
        if !is_likely_text(&cf.path) {
            if args.verbose {
                log_pb(
                    pb.as_ref(),
                    &format!("⚠️  Skipping binary file: {:?}", cf.path),
                );
            }
            stats.files_binary += 1;
            stats.files_skipped += 1;
            if let Some(ref pb) = pb {
                pb.inc(1);
            }
            continue;
        }

        let content = match fs::read_to_string(&cf.path) {
            Ok(c) => c,
            Err(e) => {
                if args.verbose {
                    log_pb(
                        pb.as_ref(),
                        &format!("⚠️  Cannot read {:?}: {}", cf.path, e),
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

        let processed = if args.clean {
            clean_content(&cf.path, &content)
        } else {
            content
        };

        if processed.trim().is_empty() {
            stats.files_skipped += 1;
            if let Some(ref pb) = pb {
                pb.inc(1);
            }
            continue;
        }

        let relative = cf.path.strip_prefix(&args.path).unwrap_or(&cf.path);
        let header = format!("\n--- FILE: {} ---\n", relative.display());
        let chunk_len = header.len() + processed.len() + 1;

        // Flush buffer if over limit
        if !buffer.is_empty() && (buffer.len() + chunk_len > args.limit) {
            let written = write_chunk(&args.out, ext_label, chunk_index, &buffer)?;
            stats.total_output_bytes += written as u64;
            stats.chunks_created = chunk_index;
            buffer.clear();
            chunk_index += 1;
        }

        buffer.push_str(&header);
        buffer.push_str(&processed);
        buffer.push('\n');
        stats.files_processed += 1;

        if let Some(ref pb) = pb {
            pb.inc(1);
        }
    }

    // Flush remaining
    if !buffer.is_empty() {
        let written = write_chunk(&args.out, ext_label, chunk_index, &buffer)?;
        stats.total_output_bytes += written as u64;
        stats.chunks_created = chunk_index;
    }

    if let Some(ref pb) = pb {
        pb.finish_with_message("Done");
    }

    println!("\n✅ {}", stats.summary());

    Ok(())
}

// ============================================================================
// FILE COLLECTION
// ============================================================================

fn collect_files(
    args: &Args,
    target_ext: Option<&str>,
) -> Result<(Vec<CollectedFile>, HashSet<String>)> {
    let mut files = Vec::new();
    let mut matched_includes = HashSet::new();

    let walker = WalkDir::new(&args.path)
        .follow_links(true)
        .into_iter()
        .filter_entry(|e| !is_excluded_entry(e, &args.exclude, args.hidden));

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let metadata = match fs::metadata(path) {
            Ok(m) => m,
            Err(_) => continue,
        };

        let path_str = path.to_string_lossy();
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();

        let mut should_process = false;

        match target_ext {
            Some(ext) => {
                // Filter by extension
                if let Some(file_ext) = path.extension() {
                    if file_ext.to_string_lossy().to_lowercase() == ext {
                        should_process = true;
                    }
                }
            }
            None => {
                // No type filter — accept all files (binary check later)
                should_process = true;
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
            if args.verbose {
                println!("   ✓ {:?} ({})", path, format_size(metadata.len()));
            }
            files.push(CollectedFile {
                path: path.to_path_buf(),
                size: metadata.len(),
            });
        }
    }

    // External includes (absolute/relative paths outside source tree)
    for inc in &args.include {
        let inc_path = Path::new(inc);
        if inc_path.exists() && inc_path.is_file() {
            let abs = fs::canonicalize(inc_path).unwrap_or_else(|_| inc_path.to_path_buf());
            let already = files.iter().any(|f| {
                fs::canonicalize(&f.path)
                    .map(|cp| cp == abs)
                    .unwrap_or(false)
            });
            if !already {
                let size = fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
                files.push(CollectedFile {
                    path: inc_path.to_path_buf(),
                    size,
                });
                matched_includes.insert(inc.clone());
                println!("   ➕ Added external include: {:?}", inc_path);
            } else {
                matched_includes.insert(inc.clone());
            }
        }
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));

    Ok((files, matched_includes))
}

// ============================================================================
// BINARY DETECTION
// ============================================================================

fn is_likely_text(path: &Path) -> bool {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };

    let mut buf = [0u8; 8192];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return false,
    };

    if n == 0 {
        return true; // empty file is text
    }

    // Count NUL bytes — if any in first 8K, likely binary
    let nul_count = buf[..n].iter().filter(|&&b| b == 0).count();
    nul_count == 0
}

// ============================================================================
// EXCLUDE / INCLUDE MATCHING
// ============================================================================

fn is_excluded_entry(entry: &DirEntry, excludes: &[String], include_hidden: bool) -> bool {
    let name = entry.file_name().to_string_lossy();

    if !include_hidden && entry.depth() > 0 && name.starts_with('.') {
        return true;
    }

    let path = entry.path();
    let path_str = path.to_string_lossy();

    for excl in excludes {
        // Exact component match
        if path.components().any(|c| c.as_os_str() == excl.as_str()) {
            return true;
        }

        // Path contains pattern (with separators)
        if (excl.contains('/') || excl.contains('\\')) && path_str.contains(excl.as_str()) {
            return true;
        }

        // Simple glob
        if excl.contains('*') {
            let pattern = excl.replace('.', r"\.").replace('*', ".*");
            if let Ok(re) = Regex::new(&format!("(?i)^{}$", pattern)) {
                if re.is_match(&name) {
                    return true;
                }
            }
        }
    }

    false
}

fn matches_include_pattern(file_name: &str, path_str: &str, pattern: &str) -> bool {
    // Exact filename
    if file_name == pattern {
        return true;
    }

    // Path suffix
    if let Some(before) = path_str.strip_suffix(pattern) {
        if before.is_empty() || before.ends_with('/') || before.ends_with('\\') {
            return true;
        }
    }

    // Normalized path contains
    if pattern.contains('/') || pattern.contains('\\') {
        let np = pattern.replace('\\', "/");
        let ns = path_str.replace('\\', "/");
        if ns.contains(&np) {
            return true;
        }
    }

    false
}

// ============================================================================
// CLEAN CONTENT
// ============================================================================

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
        | "ini" | "r" | "jl" => &SCRIPT_STYLE_REGEX,
        "php" | "php3" | "php4" | "php5" | "phtml" => &PHP_STYLE_REGEX,
        "html" | "htm" | "xml" | "xhtml" | "svg" | "vue" | "svelte" => &HTML_STYLE_REGEX,
        "sql" | "mysql" | "pgsql" | "sqlite" => &SQL_STYLE_REGEX,
        _ if file_name == "dockerfile"
            || file_name == "makefile"
            || file_name == "cmakelists.txt"
            || file_name.starts_with(".git")
            || file_name.starts_with(".docker")
            || file_name.starts_with(".env") =>
        {
            &SCRIPT_STYLE_REGEX
        }
        _ => &C_STYLE_REGEX,
    };

    let replaced = regex.replace_all(content, |caps: &Captures| {
        if let Some(m) = caps.name("keep") {
            m.as_str().to_string()
        } else {
            String::new()
        }
    });

    EMPTY_LINES_REGEX
        .replace_all(&replaced, "\n")
        .trim()
        .to_string()
}

// ============================================================================
// OUTPUT
// ============================================================================

fn generate_output_filename(pattern: &str, ext: &str, index: usize) -> String {
    let ext_clean = ext.trim_start_matches('.');

    let mut filename = pattern
        .replace("{type}", ext_clean)
        .replace("{ext}", ext_clean)
        .replace("{index}", &index.to_string());

    if filename.contains('*') {
        filename = filename.replace('*', &index.to_string());
    } else if !filename.contains(&index.to_string()) {
        let p = Path::new(&filename);
        let parent = p.parent().unwrap_or(Path::new("."));
        let stem = p.file_stem().unwrap_or_default().to_string_lossy();
        let extension = p
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

fn write_chunk(pattern: &str, ext: &str, index: usize, content: &str) -> Result<usize> {
    let filename = generate_output_filename(pattern, ext, index);
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

fn prepare_output_directory(args: &Args) -> Result<()> {
    let out_path = Path::new(&args.out);

    if let Some(parent) = out_path.parent() {
        if parent.as_os_str().is_empty() || parent == Path::new(".") {
            return Ok(());
        }

        if parent.exists() {
            // Only delete files matching the output pattern, never rm -rf
            clean_previous_output(&args.out)?;
        } else {
            fs::create_dir_all(parent)?;
        }
    }

    Ok(())
}

/// Remove only files matching the output pattern (safe, never deletes directories)
fn clean_previous_output(pattern: &str) -> Result<()> {
    let path = Path::new(pattern);
    let parent = match path.parent() {
        Some(p) if p.exists() => p,
        _ => return Ok(()),
    };

    let file_pattern = path.file_name().unwrap_or_default().to_string_lossy();

    // Build regex from the pattern's filename part
    let regex_str = format!(
        "^{}$",
        regex::escape(&file_pattern)
            .replace(r"\*", r"\d+")
            .replace(r"\{index\}", r"\d+")
            .replace(r"\{type\}", r"[a-zA-Z0-9]+")
    );

    let re = match Regex::new(&regex_str) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };

    let entries = match fs::read_dir(parent) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    let mut removed = 0;
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        if re.is_match(&name) && entry.path().is_file() {
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }

    if removed > 0 {
        println!("🗑️  Removed {} previous output file(s)", removed);
    }

    Ok(())
}

// ============================================================================
// TREE VIEW
// ============================================================================

fn generate_full_tree(args: &Args) -> Result<String> {
    let mut output = String::new();
    let mut stats = TreeStats::default();
    let mut visited = HashSet::new();

    output.push_str(&format!("PROJECT STRUCTURE: {:?}\n", args.path));
    output.push_str("==========================================\n");

    let root_name = args
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());
    output.push_str(&format!("{}/\n", root_name));

    let depth = args.effective_tree_depth();
    output.push_str(&build_tree(
        &args.path,
        &args.exclude,
        String::new(),
        0,
        depth,
        args.hidden,
        args.show_size,
        &mut visited,
        &mut stats,
    ));

    output.push_str(&stats.summary());
    output.push_str("\n==========================================\n\n");

    Ok(output)
}

#[allow(clippy::too_many_arguments)]
fn build_tree(
    dir: &Path,
    excludes: &[String],
    prefix: String,
    depth: usize,
    max_depth: usize,
    hidden: bool,
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
            if !hidden && name.starts_with('.') {
                return false;
            }
            !excludes.contains(&name)
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
            fs::metadata(&path)
                .map(|m| format!(" ({})", format_size(m.len())))
                .unwrap_or_default()
        } else {
            String::new()
        };

        let dir_marker = if is_dir { "/" } else { "" };

        output.push_str(&format!(
            "{}{}{}{}{}\n",
            prefix, connector, name, dir_marker, size_info
        ));

        if is_dir {
            stats.directories += 1;
            let child_prefix = if is_last {
                format!("{}    ", prefix)
            } else {
                format!("{}│   ", prefix)
            };
            output.push_str(&build_tree(
                &path,
                excludes,
                child_prefix,
                depth + 1,
                max_depth,
                hidden,
                show_size,
                visited,
                stats,
            ));
        } else {
            stats.files += 1;
            if let Ok(m) = fs::metadata(&path) {
                stats.total_size += m.len();
            }
        }
    }

    output
}

// ============================================================================
// CONFIG FILE
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

    // Track which args were explicitly set on CLI
    let cli_has_type = args.file_type.is_some();
    let cli_has_path = args.path != Path::new(".");
    let cli_has_out = args.out != "dump/dump_*.txt";
    let cli_has_limit = args.limit != DEFAULT_LIMIT;
    let cli_has_max_size = args.max_file_size != MAX_FILE_SIZE;

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
                "type" if !cli_has_type => {
                    if !value.is_empty() {
                        args.file_type = Some(value.to_string());
                    }
                }
                "path" if !cli_has_path => {
                    args.path = PathBuf::from(value);
                }
                "out" if !cli_has_out => {
                    args.out = value.to_string();
                }
                "limit" if !cli_has_limit => {
                    if let Ok(v) = value.parse() {
                        args.limit = v;
                    }
                }
                "max_file_size" if !cli_has_max_size => {
                    if let Ok(v) = value.parse() {
                        args.max_file_size = v;
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
                "clean" if !args.clean => {
                    args.clean = value == "true" || value == "1";
                }
                "progress" if !args.progress => {
                    args.progress = value == "true" || value == "1";
                }
                "verbose" if !args.verbose => {
                    args.verbose = value == "true" || value == "1";
                }
                "hidden" if !args.hidden => {
                    args.hidden = value == "true" || value == "1";
                }
                "no_tree" if !args.no_tree => {
                    args.no_tree = value == "true" || value == "1";
                }
                "show_size" if !args.show_size => {
                    args.show_size = value == "true" || value == "1";
                }
                "tree_depth" if args.tree_depth.is_none() => {
                    if let Ok(d) = value.parse() {
                        args.tree_depth = Some(d);
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

// ============================================================================
// SUBCOMMANDS
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
    println!("   1. Edit .dumperrc and set 'type = <ext>' (or leave empty for all files)");
    println!("   2. Customize exclude patterns");
    println!("   3. Run: source-dumper");

    Ok(())
}

fn cmd_config(args: &Args, diff_only: bool) -> Result<()> {
    let mut display = args.clone();

    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from(".dumperrc"));

    if config_path.exists() {
        load_config_file(&mut display)?;
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
        &display.path.to_string_lossy(),
        display.path == Path::new("."),
    );
    show(
        "type",
        display.file_type.as_deref().unwrap_or("(all text files)"),
        display.file_type.is_none(),
    );
    show("out", &display.out, display.out == "dump/dump_*.txt");
    show(
        "limit",
        &display.limit.to_string(),
        display.limit == DEFAULT_LIMIT,
    );
    show(
        "max_file_size",
        &format_size(display.max_file_size),
        display.max_file_size == MAX_FILE_SIZE,
    );
    show("clean", &display.clean.to_string(), !display.clean);
    show("progress", &display.progress.to_string(), !display.progress);
    show("verbose", &display.verbose.to_string(), !display.verbose);
    show("no_tree", &display.no_tree.to_string(), !display.no_tree);
    show(
        "tree_depth",
        &display
            .tree_depth
            .map(|d| d.to_string())
            .unwrap_or("unlimited".to_string()),
        display.tree_depth.is_none(),
    );
    show("hidden", &display.hidden.to_string(), !display.hidden);
    show(
        "show_size",
        &display.show_size.to_string(),
        !display.show_size,
    );

    println!();

    if !display.exclude.is_empty() {
        println!("   Excludes ({}):", display.exclude.len());
        for ex in &display.exclude {
            println!("      - {}", ex);
        }
    }
    if !display.include.is_empty() {
        println!("   Includes ({}):", display.include.len());
        for inc in &display.include {
            println!("      - {}", inc);
        }
    }

    println!("─────────────────────────────────────────");

    Ok(())
}

// ============================================================================
// UTILITIES
// ============================================================================

fn log_pb(pb: Option<&ProgressBar>, msg: &str) {
    if let Some(pb) = pb {
        pb.suspend(|| println!("{}", msg));
    } else {
        println!("{}", msg);
    }
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
    let mut result = HashSet::new();
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
    fn test_matches_include_pattern() {
        assert!(matches_include_pattern(
            "config.json",
            "/home/user/project/config.json",
            "config.json"
        ));
        assert!(matches_include_pattern(
            "config.json",
            "/home/user/project/config.json",
            "project/config.json"
        ));
        assert!(!matches_include_pattern(
            "config.json",
            "/home/user/project/config.json",
            "other.json"
        ));
    }

    #[test]
    fn test_is_likely_text() {
        // Create temp text file
        let dir = std::env::temp_dir().join("dumper_test");
        let _ = fs::create_dir_all(&dir);

        let text_file = dir.join("test.txt");
        fs::write(&text_file, "hello world\n").unwrap();
        assert!(is_likely_text(&text_file));

        let bin_file = dir.join("test.bin");
        fs::write(&bin_file, b"\x00\x01\x02\x03").unwrap();
        assert!(!is_likely_text(&bin_file));

        let empty_file = dir.join("empty.txt");
        fs::write(&empty_file, "").unwrap();
        assert!(is_likely_text(&empty_file));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_clean_content_c_style() {
        let content = "// comment\nlet x = 5; /* inline */\nlet y = \"// not a comment\";";
        let cleaned = clean_content(Path::new("test.rs"), content);
        assert!(!cleaned.contains("// comment"));
        assert!(!cleaned.contains("/* inline */"));
        assert!(cleaned.contains("// not a comment"));
    }

    #[test]
    fn test_clean_content_script_style() {
        let content = "# comment\nx = 5\ny = \"# not a comment\"";
        let cleaned = clean_content(Path::new("test.py"), content);
        assert!(!cleaned.contains("# comment"));
        assert!(cleaned.contains("# not a comment"));
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
        assert_eq!(
            generate_output_filename("dump/dump_*.txt", "all", 3),
            "dump/dump_3.txt"
        );
    }

    #[test]
    fn test_effective_tree_depth() {
        let mut args = Args::parse_from(["test"]);

        args.tree_depth = None;
        assert_eq!(args.effective_tree_depth(), MAX_TREE_DEPTH_CAP);

        args.tree_depth = Some(10);
        assert_eq!(args.effective_tree_depth(), 10);

        args.tree_depth = Some(200);
        assert_eq!(args.effective_tree_depth(), MAX_TREE_DEPTH_CAP);
    }

    #[test]
    fn test_no_type_means_all_files() {
        // When file_type is None, target_ext should be None
        let args = Args::parse_from(["test"]);
        assert!(args.file_type.is_none());
    }
}
