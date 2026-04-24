use anyhow::{Context, Result};
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use once_cell::sync::Lazy;
use regex::{Captures, Regex, RegexBuilder};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read}; // Removed unused Write
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

// ============================================================================
// CONSTANTS & TEMPLATES
// ============================================================================

const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;
const DEFAULT_TREE_DEPTH: usize = 20;
const ABSOLUTE_MAX_DEPTH: usize = 100;
const DEFAULT_LIMIT: usize = 110_000;
const DEFAULT_OUT_PATTERN: &str = "dump/dump_*.txt";

const DEFAULT_CONFIG: &str = r#"# Source Dumper Configuration (.dumperrc)

# path = .
# type = rs
# out = dump/dump_*.txt
# limit = 110000

# Exclude supports exact names, path fragments, and globs (*, ?)
# Brace expansion works in config: {target,build,*.log}
exclude = .git, target, node_modules, .idea, .vscode, *.lock, *.log

# include = .env.example, Makefile
# clean = false
# progress = true
# verbose = false
# no_tree = false
# tree_depth = 20
# show_size = false
# hidden = false
"#;

// ============================================================================
// REGEX PATTERNS
// ============================================================================

static C_STYLE_REGEX: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(r#"(?P<keep>`[^`\\]*(?:\\.[^`\\]*)*`|"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>/\*[\s\S]*?\*/|//.*)"#)
        .build().expect("C regex")
});

static SCRIPT_STYLE_REGEX: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(r#"(?P<keep>"""[\s\S]*?"""|'''[\s\S]*?'''|"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>#.*)"#)
        .build().expect("Script regex")
});

static PHP_STYLE_REGEX: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(r#"(?P<keep>`[^`\\]*(?:\\.[^`\\]*)*`|"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>/\*[\s\S]*?\*/|//.*|#.*)"#)
        .build().expect("PHP regex")
});

static SQL_STYLE_REGEX: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(r#"(?P<keep>"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>/\*[\s\S]*?\*/|--.*)"#)
        .build().expect("SQL regex")
});

static HTML_STYLE_REGEX: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(
        r#"(?P<keep>"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop><!--[\s\S]*?-->)"#,
    )
    .build()
    .expect("HTML regex")
});

static EMPTY_LINES_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)(^\s*\n)+").unwrap());
static BRACE_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(.*?)\{([^{}]+)}(.*)$").unwrap());

// ============================================================================
// MODELS & CONTEXTS
// ============================================================================

#[derive(Parser, Debug, Clone)]
#[command(
    name = "source-dumper",
    version,
    about = "Aggregate source files for LLM context"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,
    #[arg(long, default_value = ".")]
    path: PathBuf,
    #[arg(long = "type")]
    file_type: Option<String>,
    #[arg(long)]
    clean: bool,
    #[arg(long, default_value = DEFAULT_OUT_PATTERN)]
    out: String,
    #[arg(long)]
    no_clean_out: bool,
    #[arg(long)]
    progress: bool,
    #[arg(long, short)]
    verbose: bool,
    #[arg(long)]
    dry_run: bool,
    #[arg(long, default_value_t = DEFAULT_LIMIT)]
    limit: usize,
    #[arg(long, default_value_t = MAX_FILE_SIZE)]
    max_file_size: u64,
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    exclude: Vec<String>,
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    include: Vec<String>,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    tree_depth: Option<usize>,
    #[arg(long)]
    no_tree: bool,
    #[arg(long)]
    hidden: bool,
    #[arg(long)]
    show_size: bool,
    #[arg(long)]
    no_config: bool,
}

#[derive(Subcommand, Debug, Clone)]
enum Commands {
    Init {
        #[arg(long, short)]
        force: bool,
        #[arg(long, default_value = ".dumperrc")]
        output: PathBuf,
    },
    Config {
        #[arg(long)]
        diff: bool,
    },
}

struct CompiledRules {
    exclude_globs: Vec<Regex>,
    include_globs: Vec<Regex>,
}

#[derive(Debug, Clone)]
struct CollectedFile {
    path: PathBuf,
    display_path: PathBuf,
    size: u64,
}

#[derive(Default)]
struct ProcessingStats {
    files_total: usize,
    files_processed: usize,
    bytes_in: u64,
    bytes_out: u64,
    chunks: usize,
}

#[derive(Default)]
struct TreeStats {
    dirs: usize,
    files: usize,
    total_size: u64,
}

struct TreeContext<'a> {
    base: &'a Path,
    rules: &'a CompiledRules,
    max_depth: usize,
    hidden: bool,
    show_size: bool,
    visited: &'a mut HashSet<PathBuf>,
    stats: &'a mut TreeStats,
}

// ============================================================================
// MAIN
// ============================================================================

fn main() -> Result<()> {
    let matches = Args::command().get_matches();
    let mut args = Args::from_arg_matches(&matches)?;

    if !args.no_config {
        let _ = load_config_file(&mut args, &matches);
    }

    match &args.command {
        Some(Commands::Init { force, output }) => return cmd_init(*force, output),
        Some(Commands::Config { diff }) => return cmd_config(&args, *diff, &matches),
        None => {}
    }

    let base_path = fs::canonicalize(&args.path).context("Source path not found")?;

    let rules = CompiledRules {
        exclude_globs: expand_braces(&args.exclude)
            .into_iter()
            .flat_map(|s| glob_to_regex(&s))
            .collect(),
        include_globs: expand_braces(&args.include)
            .into_iter()
            .flat_map(|s| glob_to_regex(&s))
            .collect(),
    };

    if !args.dry_run {
        prepare_output_directory(&args)?;
    }

    let (files, _) = collect_files(&args, &base_path, &rules)?;

    if files.is_empty() {
        println!("No files found to process.");
        return Ok(());
    }

    if args.dry_run {
        println!("🔍 Dry run: Found {} files.", files.len());
        return Ok(());
    }

    process_files(&args, &files, &base_path, &rules)?;

    Ok(())
}

// ============================================================================
// LOGIC
// ============================================================================

fn collect_files(
    args: &Args,
    base_path: &Path,
    rules: &CompiledRules,
) -> Result<(Vec<CollectedFile>, HashSet<usize>)> {
    let mut files = Vec::new();
    let mut matched_indices = HashSet::new();
    let mut visited = HashSet::new();
    let target_ext = args
        .file_type
        .as_ref()
        .map(|s| s.trim_start_matches('.').to_lowercase());

    let walker = WalkDir::new(base_path)
        .follow_links(true)
        .into_iter()
        .filter_entry(|e| {
            if e.file_type().is_dir() {
                match fs::canonicalize(e.path()) {
                    Ok(c) => {
                        if !visited.insert(c) {
                            return false;
                        }
                    }
                    Err(_) => return false,
                }
            }
            !is_excluded(e.path(), base_path, rules, args.hidden)
        });

    for entry in walker.filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        let rel_path = path.strip_prefix(base_path).unwrap_or(path);
        let mut should_add = false;

        if let Some(ref target) = target_ext {
            if path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase() == *target)
                .unwrap_or(false)
            {
                should_add = true;
            }
        } else {
            should_add = true;
        }

        for (i, re) in rules.include_globs.iter().enumerate() {
            if re.is_match(&rel_path.to_string_lossy()) {
                should_add = true;
                matched_indices.insert(i);
            }
        }

        if should_add {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            files.push(CollectedFile {
                path: path.to_path_buf(),
                display_path: rel_path.to_path_buf(),
                size,
            });
        }
    }

    for (i, inc) in args.include.iter().enumerate() {
        let p = Path::new(inc);
        if p.is_file() && !matched_indices.contains(&i) {
            let size = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            files.push(CollectedFile {
                path: p.to_path_buf(),
                display_path: PathBuf::from(format!(
                    "[external]/{}",
                    p.file_name().unwrap_or_default().to_string_lossy()
                )),
                size,
            });
            matched_indices.insert(i);
        }
    }

    files.sort_by(|a, b| a.display_path.cmp(&b.display_path));
    Ok((files, matched_indices))
}

fn is_excluded(path: &Path, base: &Path, rules: &CompiledRules, include_hidden: bool) -> bool {
    let rel_path = path.strip_prefix(base).unwrap_or(path).to_string_lossy();
    for re in &rules.exclude_globs {
        if re.is_match(&rel_path) {
            return true;
        }
    }

    if !include_hidden {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        if name.starts_with('.') && path != base {
            return true;
        }
    }
    false
}

fn process_files(
    args: &Args,
    files: &[CollectedFile],
    base: &Path,
    rules: &CompiledRules,
) -> Result<()> {
    let mut buffer = String::with_capacity(args.limit);
    let mut stats = ProcessingStats {
        files_total: files.len(),
        ..Default::default()
    };

    let pb = if args.progress {
        let p = ProgressBar::new(files.len() as u64);
        p.set_style(
            ProgressStyle::default_bar()
                .template(
                    "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
                )
                .unwrap()
                .progress_chars("#>-"),
        );
        Some(p)
    } else {
        None
    };

    if !args.no_tree {
        buffer.push_str(&generate_tree(args, base, rules));
    }

    let type_label = args.file_type.as_deref().unwrap_or("all");

    for cf in files {
        if let Some(ref p) = pb {
            p.inc(1);
        }
        if cf.size > args.max_file_size || !is_likely_text(&cf.path) {
            continue;
        }

        let content = match fs::read_to_string(&cf.path) {
            Ok(s) => s,
            Err(_) => {
                if args.verbose {
                    println!("⚠️  Skipping non-UTF8: {:?}", cf.display_path);
                }
                continue;
            }
        };

        stats.bytes_in += content.len() as u64;
        let processed = if args.clean {
            clean_content(&cf.path, &content)
        } else {
            content
        };

        let header = format!("\n--- FILE: {} ---\n", cf.display_path.display());
        if !buffer.is_empty() && (buffer.len() + header.len() + processed.len() > args.limit) {
            stats.bytes_out += buffer.len() as u64;
            stats.chunks += 1;
            write_chunk(&args.out, type_label, stats.chunks, &buffer)?;
            buffer.clear();
        }
        buffer.push_str(&header);
        buffer.push_str(&processed);
        buffer.push('\n');
        stats.files_processed += 1;
    }

    if !buffer.is_empty() {
        stats.bytes_out += buffer.len() as u64;
        stats.chunks += 1;
        write_chunk(&args.out, type_label, stats.chunks, &buffer)?;
    }

    if let Some(ref p) = pb {
        p.finish_and_clear();
    }
    println!(
        "\n✅ Processed {}/{} files ({} -> {}) into {} chunks.",
        stats.files_processed,
        stats.files_total,
        format_size(stats.bytes_in),
        format_size(stats.bytes_out),
        stats.chunks
    );
    Ok(())
}

// ============================================================================
// HELPERS
// ============================================================================

fn glob_to_regex(pattern: &str) -> Option<Regex> {
    let mut re = String::from("(?i)");
    re.push_str(r"(^|[\\/])");
    for ch in pattern.chars() {
        match ch {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '.' | '+' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' | '\\' => {
                re.push('\\');
                re.push(ch);
            }
            '/' => re.push_str(r"[\\/]"),
            _ => re.push(ch),
        }
    }
    re.push_str(r"($|[\\/])");
    Regex::new(&re).ok()
}

fn expand_braces(patterns: &[String]) -> Vec<String> {
    let mut expanded = Vec::new();
    for p in patterns {
        expand_recursive(p, &mut expanded);
    }
    expanded
}

fn expand_recursive(p: &str, out: &mut Vec<String>) {
    if let Some(caps) = BRACE_REGEX.captures(p) {
        for part in caps[2].split(',') {
            expand_recursive(&format!("{}{}{}", &caps[1], part.trim(), &caps[3]), out);
        }
    } else {
        out.push(p.to_string());
    }
}

fn is_likely_text(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 1024];
    let n = f.read(&mut buf).unwrap_or(0);
    !buf[..n].contains(&0)
}

fn clean_content(path: &Path, content: &str) -> String {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let regex = match ext.as_str() {
        "py" | "rb" | "sh" | "yml" | "yaml" | "toml" | "env" => &*SCRIPT_STYLE_REGEX,
        "php" => &*PHP_STYLE_REGEX,
        "sql" => &*SQL_STYLE_REGEX,
        "html" | "xml" | "svg" => &*HTML_STYLE_REGEX,
        _ if name == "dockerfile" || name == "makefile" => &*SCRIPT_STYLE_REGEX,
        _ => &*C_STYLE_REGEX,
    };
    let cleaned = regex.replace_all(content, |caps: &Captures| {
        caps.name("keep")
            .map(|m| m.as_str())
            .unwrap_or("")
            .to_string()
    });
    EMPTY_LINES_REGEX
        .replace_all(&cleaned, "\n")
        .trim()
        .to_string()
}

fn write_chunk(pattern: &str, file_type: &str, index: usize, content: &str) -> Result<()> {
    let path_str = pattern
        .replace("{index}", &index.to_string())
        .replace("{type}", file_type)
        .replace('*', &index.to_string());
    let path = PathBuf::from(path_str);
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    fs::write(&path, content)?;
    Ok(())
}

fn prepare_output_directory(args: &Args) -> Result<()> {
    let path = Path::new(&args.out);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if parent.exists() && !args.no_clean_out {
        let file_pattern = path.file_name().unwrap_or_default().to_string_lossy();
        let safe_regex = format!(
            "^{}$",
            regex::escape(&file_pattern)
                .replace(r"\{index\}", r"\d+")
                .replace(r"\{type\}", r"[a-zA-Z0-9_-]+")
                .replace(r"\*", r"\d+")
        );
        if let Ok(re) = Regex::new(&safe_regex) {
            for entry in fs::read_dir(parent)?.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if re.is_match(&name) {
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }
        }
    } else if !parent.exists() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn load_config_file(args: &mut Args, matches: &ArgMatches) -> Result<()> {
    let path = args
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from(".dumperrc"));
    if !path.exists() {
        return Ok(());
    }
    let file = File::open(path)?;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (key, val) = (k.trim(), v.trim().trim_matches('"'));
        let cli =
            |id: &str| matches.value_source(id) == Some(clap::parser::ValueSource::CommandLine);
        match key {
            "path" if !cli("path") => args.path = PathBuf::from(val),
            "type" if !cli("file_type") => args.file_type = Some(val.to_string()),
            "out" if !cli("out") => args.out = val.to_string(),
            "limit" if !cli("limit") => {
                if let Ok(l) = val.parse() {
                    args.limit = l
                }
            }
            "max_file_size" if !cli("max_file_size") => {
                if let Ok(s) = val.parse() {
                    args.max_file_size = s
                }
            }
            "tree_depth" if !cli("tree_depth") => {
                if let Ok(d) = val.parse() {
                    args.tree_depth = Some(d)
                }
            }
            "exclude" => args
                .exclude
                .extend(val.split(',').map(|s| s.trim().to_string())),
            "include" => args
                .include
                .extend(val.split(',').map(|s| s.trim().to_string())),
            "clean" if !args.clean => args.clean = val == "true",
            "progress" if !args.progress => args.progress = val == "true",
            "verbose" if !args.verbose => args.verbose = val == "true",
            "hidden" if !args.hidden => args.hidden = val == "true",
            "show_size" if !args.show_size => args.show_size = val == "true",
            "no_tree" if !args.no_tree => args.no_tree = val == "true",
            _ => {}
        }
    }
    Ok(())
}

// ============================================================================
// TREE LOGIC
// ============================================================================

fn generate_tree(args: &Args, base: &Path, rules: &CompiledRules) -> String {
    let mut stats = TreeStats::default();
    let mut visited = HashSet::new();
    let max = args
        .tree_depth
        .unwrap_or(DEFAULT_TREE_DEPTH)
        .min(ABSOLUTE_MAX_DEPTH);

    let mut ctx = TreeContext {
        base,
        rules,
        max_depth: max,
        hidden: args.hidden,
        show_size: args.show_size,
        visited: &mut visited,
        stats: &mut stats,
    };

    let body = walk_tree(base, "", 0, &mut ctx);

    format!(
        "PROJECT STRUCTURE: {:?}\n{}\n{}\n{}\n{} dirs, {} files, {} total\n{}\n",
        base,
        "=".repeat(40),
        body.trim_end(),
        "=".repeat(40),
        ctx.stats.dirs,
        ctx.stats.files,
        format_size(ctx.stats.total_size),
        "=".repeat(40)
    )
}

fn walk_tree(dir: &Path, prefix: &str, depth: usize, ctx: &mut TreeContext) -> String {
    if depth > ctx.max_depth {
        return format!("{}... (max depth)\n", prefix);
    }
    if let Ok(c) = fs::canonicalize(dir) {
        if !ctx.visited.insert(c) {
            return String::new();
        }
    }

    let mut out = String::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    let mut entries: Vec<_> = entries
        .flatten()
        .filter(|e| !is_excluded(&e.path(), ctx.base, ctx.rules, ctx.hidden))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let count = entries.len();
    for (i, e) in entries.into_iter().enumerate() {
        let is_last = i == count - 1;
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();

        let size = path.metadata().map(|m| m.len()).unwrap_or(0);
        let size_info = if ctx.show_size && path.is_file() {
            format!(" ({})", format_size(size))
        } else {
            String::new()
        };

        out.push_str(&format!(
            "{}{}{}{}{}\n",
            prefix,
            if is_last { "└── " } else { "├── " },
            name,
            if path.is_dir() { "/" } else { "" },
            size_info
        ));

        if path.is_dir() {
            ctx.stats.dirs += 1;
            let next_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
            out.push_str(&walk_tree(&path, &next_prefix, depth + 1, ctx));
        } else {
            ctx.stats.files += 1;
            ctx.stats.total_size += size;
        }
    }
    out
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

// ============================================================================
// SUBCOMMANDS
// ============================================================================

fn cmd_init(force: bool, output: &Path) -> Result<()> {
    if output.exists() && !force {
        println!("File exists. Use --force.");
        return Ok(());
    }
    fs::write(output, DEFAULT_CONFIG)?;
    println!("Created config at default .dumperrc location.");
    Ok(())
}

fn cmd_config(args: &Args, diff: bool, matches: &ArgMatches) -> Result<()> {
    println!("Active Configuration (* = overridden by CLI):");
    let cli = |id: &str| {
        if matches.value_source(id) == Some(clap::parser::ValueSource::CommandLine) {
            "*"
        } else {
            " "
        }
    };
    let print = |k: &str, v: String, id: &str| {
        if !diff || cli(id) == "*" {
            println!("{} {:15} = {}", cli(id), k, v);
        }
    };
    print("path", format!("{:?}", args.path), "path");
    print("type", format!("{:?}", args.file_type), "file_type");
    print("out", args.out.clone(), "out");
    print("limit", args.limit.to_string(), "limit");
    print("clean", args.clean.to_string(), "clean");
    println!("   Excludes: {:?}", args.exclude);
    Ok(())
}
