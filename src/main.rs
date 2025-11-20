use anyhow::{Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use regex::{Captures, Regex};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Source directory path to search
    #[arg(long, default_value = ".")]
    path: PathBuf,

    /// Main file extension to filter (e.g., .php, .rs)
    #[arg(long, value_name = "EXTENSION")]
    type_: String,

    /// Clean content (remove comments and empty lines)
    #[arg(long)]
    clean: bool,

    /// Output path pattern (e.g. "dump/dump_*.txt")
    #[arg(long, default_value = "dump/dump_*.txt")]
    out: String,

    /// Show progress bar
    #[arg(long)]
    progress: bool,

    /// Character limit per output file
    #[arg(long, default_value_t = 110000)]
    limit: usize,

    /// Exclude paths/folders. Matches exact directory names or path segments.
    /// Supports brace expansion: site03/{public,console}
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    exclude: Vec<String>,

    /// Read exclude patterns from file(s)
    #[arg(long)]
    exclude_file: Vec<PathBuf>,

    /// Also include these specific files or substrings
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    include: Vec<String>,

    /// Read include patterns from file(s)
    #[arg(long)]
    include_file: Vec<PathBuf>,
}

fn main() -> Result<()> {
    let mut args = Args::parse();

    if let Ok(p) = fs::canonicalize(&args.path) {
        args.path = p;
    }

    // --- 0. PRE-PROCESS ARGS ---
    if !args.include_file.is_empty() {
        args.include.extend(load_patterns_from_files(&args.include_file)?);
    }
    if !args.exclude_file.is_empty() {
        args.exclude.extend(load_patterns_from_files(&args.exclude_file)?);
    }

    args.include = expand_brace_patterns(args.include);
    args.exclude = expand_brace_patterns(args.exclude);

    // --- 1. OUTPUT DIRECTORY SAFETY CHECK ---
    let out_path_obj = Path::new(&args.out);
    if let Some(parent_dir) = out_path_obj.parent() {
        if parent_dir.exists() && parent_dir != Path::new(".") {
            let abs_out = fs::canonicalize(parent_dir).unwrap_or(parent_dir.to_path_buf());
            let abs_src = fs::canonicalize(&args.path).unwrap_or(args.path.clone());

            // Safety: Only wipe if output is NOT source and source is NOT inside output
            let is_safe = abs_out != abs_src && !abs_src.starts_with(&abs_out);

            if is_safe {
                println!("!!! WIPING DIRECTORY: {:?} !!!", parent_dir);
                fs::remove_dir_all(parent_dir).context("Failed to delete output directory")?;
                fs::create_dir_all(parent_dir).context("Failed to recreate output directory")?;
            } else {
                println!("⚠️  Skipping deletion: Output folder matches Source or contains Source.");
            }
        } else if !parent_dir.exists() && !parent_dir.as_os_str().is_empty() {
            fs::create_dir_all(parent_dir)?;
        }
    }

    let target_ext = args.type_.trim_start_matches('.').to_lowercase();
    let display_ext = format!(".{}", target_ext);

    println!(
        "Scanning: {:?} | Type: {} | Includes: {}",
        args.path,
        display_ext,
        args.include.len()
    );

    // --- 2. COMPILE REGEXES (Standard Mode - No (?x)) ---

    // C-Style: // and /* */. Supports backticks for JS/Go.
    let c_style_regex = Regex::new(
        r#"(?P<keep>`[^`\\]*(?:\\.[^`\\]*)*`|"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>/\*[\s\S]*?\*/|//.*)"#,
    )?;

    // Script-Style: # only.
    let script_style_regex = Regex::new(
        r#"(?P<keep>"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>#.*)"#,
    )?;

    // PHP-Style: //, /* */, AND #. Supports backticks.
    // Note: # matches literal hash in standard regex mode.
    let php_style_regex = Regex::new(
        r#"(?P<keep>`[^`\\]*(?:\\.[^`\\]*)*`|"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>/\*[\s\S]*?\*/|//.*|#.*)"#,
    )?;

    // Matches one or more empty lines
    let empty_lines_regex = Regex::new(r"(?m)(^\s*\n)+")?;

    // --- 3. COLLECT FILES ---
    let mut files_to_process = Vec::new();
    let mut matched_includes: HashSet<String> = HashSet::new();

    let walker = WalkDir::new(&args.path)
        .follow_links(true)
        .into_iter()
        .filter_entry(|e| !is_excluded_entry(e, &args.exclude));

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() {
            let path_str = path.to_string_lossy();
            let file_name = path.file_name().unwrap_or_default().to_string_lossy();

            let mut should_process = false;

            if let Some(ext) = path.extension() {
                if ext.to_string_lossy().to_lowercase() == target_ext {
                    should_process = true;
                }
            }

            for inc in &args.include {
                if file_name == *inc || path_str.ends_with(inc) {
                    matched_includes.insert(inc.clone());
                    should_process = true;
                }
            }

            if should_process {
                files_to_process.push(path.to_path_buf());
            }
        }
    }

    // External Includes
    for inc in &args.include {
        let inc_path = Path::new(inc);
        if inc_path.exists() && inc_path.is_file() {
            let abs_inc = fs::canonicalize(inc_path).unwrap_or(inc_path.to_path_buf());
            let already_exists = files_to_process.iter().any(|p| {
                fs::canonicalize(p).map(|cp| cp == abs_inc).unwrap_or(false)
            });

            if !already_exists {
                files_to_process.push(inc_path.to_path_buf());
                matched_includes.insert(inc.clone());
                println!("(+) Added external include: {:?}", inc_path);
            } else {
                matched_includes.insert(inc.clone());
            }
        }
    }

    if !args.include.is_empty() {
        let missing_count = args.include.len() - matched_includes.len();
        if missing_count > 0 {
            println!("⚠️  WARNING: {} include patterns were not matched.", missing_count);
        }
    }

    let total_files = files_to_process.len() as u64;
    println!("Found {} files to process.", total_files);

    if total_files == 0 {
        return Ok(());
    }

    // --- 4. PROCESS FILES ---
    let pb = if args.progress {
        let p = ProgressBar::new(total_files);
        p.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
            .progress_chars("#>-"));
        Some(p)
    } else {
        None
    };

    let mut current_buffer = String::with_capacity(args.limit);
    let mut file_part_index = 1;

    // Tree View
    current_buffer.push_str(&format!("PROJECT STRUCTURE: {:?}\n", args.path));
    current_buffer.push_str("==========================================\n");
    let mut visited_dirs = HashSet::new();

    // --- UPDATE THIS LINE BELOW ---
    current_buffer.push_str(&generate_tree_view(
        &args.path,
        &args.exclude,
        "".to_string(), // Initial prefix is empty
        0,              // Initial depth is 0
        &mut visited_dirs
    ));
    // ------------------------------

    current_buffer.push_str("\n==========================================\n\n");
    for file_path in files_to_process {
        let content = match fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(_) => {
                if let Some(pb) = &pb { pb.inc(1); }
                continue;
            }
        };

        let processed_content = if args.clean {
            let current_ext = file_path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();

            let file_name = file_path.file_name().unwrap_or_default().to_string_lossy();

            let target_regex = match current_ext.as_str() {
                "py" | "rb" | "pl" | "sh" | "yaml" | "yml" | "env" | "toml" | "dockerfile" | "makefile" => &script_style_regex,
                "php" => &php_style_regex,
                _ if file_name == "Dockerfile" || file_name == "Makefile" => &script_style_regex,
                _ => &c_style_regex,
            };

            let temp_text = target_regex.replace_all(&content, |caps: &Captures| {
                if let Some(m) = caps.name("keep") {
                    m.as_str().to_string()
                } else {
                    String::new()
                }
            });

            empty_lines_regex.replace_all(&temp_text, "\n").trim().to_string()
        } else {
            content
        };

        if !processed_content.is_empty() {
            let header = format!("\n--- FILE: {:?} ---\n", file_path);
            let chunk_len = header.len() + processed_content.len() + 1;

            if !current_buffer.is_empty() && (current_buffer.len() + chunk_len > args.limit) {
                write_to_disk(&args.out, &display_ext, file_part_index, &current_buffer)?;
                current_buffer.clear();
                file_part_index += 1;
            }

            current_buffer.push_str(&header);
            current_buffer.push_str(&processed_content);
            current_buffer.push('\n');
        }

        if let Some(pb) = &pb { pb.inc(1); }
    }

    if !current_buffer.is_empty() {
        write_to_disk(&args.out, &display_ext, file_part_index, &current_buffer)?;
    }

    if let Some(pb) = &pb { pb.finish_with_message("Done"); }
    println!("Processing complete. Parts created: {}", file_part_index);

    Ok(())
}

// --- HELPER FUNCTIONS ---

fn is_excluded_entry(entry: &DirEntry, excludes: &[String]) -> bool {
    let path = entry.path();
    let path_str = path.to_string_lossy();

    // Skip .git specifically
    if entry.depth() > 0 && entry.file_name().to_string_lossy() == ".git" {
        return true;
    }

    for excl in excludes {
        // Component match (e.g., "node_modules")
        if path.components().any(|c| c.as_os_str() == excl.as_str()) {
            return true;
        }
        // Path segment match (e.g., "vendor/bin")
        if (excl.contains('/') || excl.contains('\\')) && path_str.contains(excl) {
            return true;
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
    let brace_re = Regex::new(r"^(.*?)\{([^{}]+)}(.*)$").unwrap();
    let mut processed = Vec::new();
    let mut queue = patterns;

    while let Some(pattern) = queue.pop() {
        if let Some(caps) = brace_re.captures(&pattern) {
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
    let mut unique_set: HashSet<String> = HashSet::from_iter(processed);
    let mut result: Vec<String> = unique_set.drain().collect();
    result.sort();
    result
}

fn write_to_disk(out_pattern: &str, ext: &str, index: usize, content: &str) -> Result<()> {
    let mut filename = out_pattern.replace("{type}", ext);

    if filename.contains('*') {
        filename = filename.replace("*", &index.to_string());
    } else {
        let path_obj = Path::new(&filename);
        let parent = path_obj.parent().unwrap_or(Path::new("."));
        let stem = path_obj.file_stem().unwrap_or_default().to_string_lossy();
        let extension = path_obj.extension().map(|e| e.to_string_lossy()).unwrap_or_default();

        let new_name = if extension.is_empty() {
            format!("{}_{}", stem, index)
        } else {
            format!("{}_{}.{}", stem, index, extension)
        };
        filename = parent.join(new_name).to_string_lossy().to_string();
    }

    let path = PathBuf::from(&filename);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = File::create(&path).context(format!("Failed to create output file {:?}", path))?;
    file.write_all(content.as_bytes())?;
    println!("Saved chunk: {:?}", path);
    Ok(())
}

fn generate_tree_view(
    dir: &Path,
    excludes: &[String],
    prefix: String, // CHANGED: We pass the visual prefix string
    depth: usize,   // We keep depth only for safety limit
    visited: &mut HashSet<PathBuf>,
) -> String {
    // Safety limit to prevent stack overflow or massive outputs
    if depth > 20 {
        return String::new();
    }

    // Cycle detection
    if let Ok(canonical) = fs::canonicalize(dir) {
        if !visited.insert(canonical) {
            return String::new();
        }
    }

    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return String::new(),
    };

    // Collect and Filter
    let mut entries: Vec<_> = read_dir
        .filter_map(|e| e.ok())
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            // Filter hidden files (optional, mimics standard tree)
            if name.starts_with('.') { return false; }
            // Check user excludes
            if excludes.iter().any(|ex| name == *ex) { return false; }
            true
        })
        .collect();

    // Sort: Directories first, then alphabetical
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
    let last_index = entries.len().saturating_sub(1);

    for (i, entry) in entries.into_iter().enumerate() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();

        let is_last = i == last_index;

        // 1. Determine the connector for THIS item
        let connector = if is_last { "└── " } else { "├── " };

        output.push_str(&format!("{}{}{}\n", prefix, connector, name));

        if path.is_dir() {
            // 2. Prepare the prefix for CHILDREN
            // If this was the last item, children get empty space.
            // If this was NOT the last item, children get a vertical bar to connect next items.
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
                visited
            ));
        }
    }
    output
}
// --- TESTS ---
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_c_style_regex() {
        // Standard mode regex for C-Style
        let regex = Regex::new(
            r#"(?P<keep>`[^`\\]*(?:\\.[^`\\]*)*`|"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*')|(?P<drop>/\*[\s\S]*?\*/|//.*)"#,
        ).unwrap();

        let input = r#"
            var x = 1; // DELETE_ME_COMMENT
            var y = "string with // KEEP_ME_STRING inside";
            var z = `template with // KEEP_ME_TEMPLATE inside`;
            /*
               DELETE_ME_BLOCK
            */
            var w = 'single quote // KEEP_ME_SINGLE';
        "#;

        let output = regex.replace_all(input, |caps: &Captures| {
            if let Some(m) = caps.name("keep") { m.as_str().to_string() } else { String::new() }
        });

        assert!(output.contains("var x = 1;"));
        assert!(!output.contains("DELETE_ME_COMMENT"));
        assert!(!output.contains("DELETE_ME_BLOCK"));

        assert!(output.contains("KEEP_ME_STRING"));
        assert!(output.contains("KEEP_ME_TEMPLATE"));
        assert!(output.contains("KEEP_ME_SINGLE"));
    }

    #[test]
    fn test_brace_expansion() {
        let input = vec!["src/{a,b,c}.rs".to_string(), "test".to_string()];
        let output = expand_brace_patterns(input);
        assert!(output.contains(&"src/a.rs".to_string()));
        assert!(output.contains(&"src/b.rs".to_string()));
        assert!(output.contains(&"src/c.rs".to_string()));
        assert!(output.contains(&"test".to_string()));
    }

    #[test]
    fn test_smart_exclusion() {
        let excludes = vec!["test".to_string(), "vendor/bin".to_string()];

        let p1 = PathBuf::from("src/app/test/file.php");
        let p2 = PathBuf::from("src/app/latest_news.php");
        let p3 = PathBuf::from("src/vendor/bin/tool");

        let check = |path: &PathBuf| -> bool {
            let path_str = path.to_string_lossy();
            for excl in &excludes {
                if path.components().any(|c| c.as_os_str() == excl.as_str()) { return true; }
                if (excl.contains('/') || excl.contains('\\')) && path_str.contains(excl) { return true; }
            }
            false
        };

        assert!(check(&p1));
        assert!(!check(&p2));
        assert!(check(&p3));
    }
}