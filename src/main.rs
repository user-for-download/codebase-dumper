use anyhow::{Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use regex::{Captures, Regex};
use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Source directory path to search
    #[arg(long)]
    path: PathBuf,

    /// Main file extension to filter (e.g., .php)
    #[arg(long, value_name = "EXTENSION")]
    type_: String,

    /// Clean content (remove comments and empty lines)
    #[arg(long)]
    clean: bool,

    /// Output path pattern (e.g. "site03/dump/dump_*.txt")
    #[arg(long)]
    out: String,

    /// Show progress bar
    #[arg(long)]
    progress: bool,

    /// Character limit per output file
    #[arg(long, default_value_t = 110000)]
    limit: usize,

    /// Exclude paths containing these strings (comma separated)
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    exclude: Vec<String>,

    /// Also include these specific files or substrings (comma separated)
    /// Can be file names (.env) or full paths (site03/.env)
    #[arg(long, value_delimiter = ',', num_args = 1..)]
    include: Vec<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // --- 1. OUTPUT DIRECTORY SAFETY CHECK ---
    let out_path_obj = Path::new(&args.out);
    if let Some(parent_dir) = out_path_obj.parent() {
        if parent_dir.exists() && !parent_dir.as_os_str().is_empty() && parent_dir != Path::new(".") {
            let mut is_safe = true;
            if let Ok(abs_out) = fs::canonicalize(parent_dir) {
                // If source path exists, check safety
                if let Ok(abs_src) = fs::canonicalize(&args.path) {
                    if abs_src.starts_with(&abs_out) { is_safe = false; }
                }
            }

            if is_safe {
                println!("!!! WIPING DIRECTORY: {:?} !!!", parent_dir);
                fs::remove_dir_all(parent_dir).context("Failed to delete output directory")?;
                fs::create_dir_all(parent_dir).context("Failed to recreate output directory")?;
            } else {
                println!("⚠️  Skipping deletion: Output folder contains Source folder.");
            }
        } else if !parent_dir.exists() && !parent_dir.as_os_str().is_empty() {
            fs::create_dir_all(parent_dir)?;
        }
    }

    // Normalize target extension
    let target_ext = args.type_.trim_start_matches('.').to_lowercase();
    let display_ext = format!(".{}", target_ext);

    println!("Scanning: {:?} | Type: {} | Includes: {:?}", args.path, display_ext, args.include);

    // --- 2. COLLECT FILES ---
    let mut files_to_process = Vec::new();

    // Trackers for validation
    let mut matched_includes: HashSet<String> = HashSet::new();
    let mut matched_excludes: HashSet<String> = HashSet::new();

    // A. SCAN MAIN DIRECTORY
    for entry in WalkDir::new(&args.path).follow_links(true).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() {
            let path_str = path.to_string_lossy();
            let file_name = path.file_name().unwrap_or_default().to_string_lossy();

            // Exclude Logic
            let mut is_excluded = false;
            for excl in &args.exclude {
                if path_str.contains(excl) {
                    matched_excludes.insert(excl.clone());
                    is_excluded = true;
                }
            }
            if is_excluded { continue; }

            // Match Logic
            let mut should_process = false;

            // Check 1: Main Extension
            if let Some(ext) = path.extension() {
                if ext.to_string_lossy().to_lowercase() == target_ext {
                    should_process = true;
                }
            }

            // Check 2: Include List (Filename or EndsWith)
            if !args.include.is_empty() {
                for inc in &args.include {
                    if file_name == *inc || path_str.ends_with(inc) {
                        matched_includes.insert(inc.clone());
                        should_process = true;
                    }
                }
            }

            if should_process {
                files_to_process.push(path.to_path_buf());
            }
        }
    }

    // B. HANDLE EXTERNAL INCLUDES (Fix for site03/.env)
    // If an include path was explicitly provided and exists on disk, add it
    // (even if it wasn't found in the scan directory).
    for inc in &args.include {
        let inc_path = Path::new(inc);
        if inc_path.exists() && inc_path.is_file() {

            // Resolve to absolute to prevent duplicates
            let abs_inc = match fs::canonicalize(inc_path) {
                Ok(p) => p,
                Err(_) => continue
            };

            // Check if we already have it
            let already_added = files_to_process.iter().any(|p| {
                if let Ok(abs_p) = fs::canonicalize(p) {
                    abs_p == abs_inc
                } else {
                    false
                }
            });

            if !already_added {
                files_to_process.push(inc_path.to_path_buf());
                matched_includes.insert(inc.clone());
                println!("(+) Added external include: {:?}", inc_path);
            } else {
                // It was already found in the scan, just mark matched
                matched_includes.insert(inc.clone());
            }
        }
    }

    // --- 3. VALIDATION WARNINGS ---
    for req_inc in &args.include {
        if !matched_includes.contains(req_inc) {
            println!("⚠️  WARNING: Include pattern '{}' was NOT found.", req_inc);
        }
    }

    let total_files = files_to_process.len() as u64;
    println!("Found {} files to process.", total_files);

    if total_files == 0 {
        return Ok(());
    }

    // 4. Setup Progress Bar
    let pb = if args.progress {
        let pb = ProgressBar::new(total_files);
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")?
            .progress_chars("#>-"));
        Some(pb)
    } else {
        None
    };

    // 5. Prepare Cleaners
    let c_style_regex = Regex::new(r#"(?x)("[^"\\]*(\\.[^"\\]*)*")|(/\*[\s\S]*?\*/)|(//.*)"#)?;
    let script_style_regex = Regex::new(r#"(?x)("[^"\\]*(\\.[^"\\]*)*")|('[^'\\]*(\\.[^'\\]*)*')|(\#.*)"#)?;
    let empty_lines = Regex::new(r"(?m)^\s*$\n?")?;

    // 6. Processing Loop
    let mut current_buffer = String::with_capacity(args.limit);
    let mut file_part_index = 1;

    // Add Tree View
    current_buffer.push_str(&format!("PROJECT STRUCTURE: {:?}\n", args.path));
    current_buffer.push_str("==========================================\n");
    current_buffer.push_str(&generate_tree_view(&args.path, &args.exclude, 0));
    current_buffer.push_str("\n==========================================\n\n");

    for file_path in files_to_process {
        let content = match read_file_content(&file_path) {
            Ok(c) => c,
            Err(_) => {
                if let Some(pb) = &pb { pb.inc(1); }
                continue;
            }
        };

        let processed_content = if args.clean {
            let mut temp_text = Cow::from(content);

            // Dynamic Comment Style
            let current_ext = file_path.extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();

            // Check extension OR exact filename for script style
            let file_name = file_path.file_name().unwrap_or_default().to_string_lossy();
            let use_script_style = matches!(current_ext.as_str(),
                "py" | "rb" | "pl" | "sh" | "yaml" | "yml" | "env" | ""
            ) || file_name == "Dockerfile" || file_name == ".env" || file_name == "Makefile";

            if use_script_style {
                temp_text = script_style_regex.replace_all(&temp_text, |caps: &Captures| {
                    if let Some(m) = caps.get(1) { return m.as_str().to_string(); }
                    if let Some(m) = caps.get(2) { return m.as_str().to_string(); }
                    "".to_string()
                }).into_owned().into();
            } else {
                temp_text = c_style_regex.replace_all(&temp_text, |caps: &Captures| {
                    if let Some(m) = caps.get(1) { return m.as_str().to_string(); }
                    "".to_string()
                }).into_owned().into();
            }
            empty_lines.replace_all(&temp_text, "\n").trim().to_string()
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

fn read_file_content(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
}

fn write_to_disk(out_pattern: &str, ext: &str, index: usize, content: &str) -> Result<()> {
    let mut filename = out_pattern.replace("{type}", ext);

    if filename.contains('*') {
        filename = filename.replace("*", &index.to_string());
    } else {
        let path_obj = Path::new(&filename);
        let parent = path_obj.parent().unwrap_or(Path::new("."));
        let stem = path_obj.file_stem().unwrap_or_default().to_string_lossy();
        let extension = path_obj.extension().unwrap_or_default().to_string_lossy();
        let new_name = format!("{}_{}.{}", stem, index, extension);
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

fn generate_tree_view(dir: &Path, excludes: &[String], depth: usize) -> String {
    let mut output = String::new();
    if depth > 20 { return output; }
    let read_dir = match fs::read_dir(dir) { Ok(rd) => rd, Err(_) => return output };

    let mut entries: Vec<_> = read_dir.filter_map(|e| e.ok()).collect();
    entries.sort_by(|a, b| {
        let a_dir = a.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let b_dir = b.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if a_dir == b_dir { a.file_name().cmp(&b.file_name()) } else { b_dir.cmp(&a_dir) }
    });

    for (i, entry) in entries.iter().enumerate() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if excludes.iter().any(|e| name.contains(e)) { continue; }
        if name == ".git" { continue; }

        let is_last = i == entries.len() - 1;
        let prefix = "    ".repeat(depth);
        let marker = if is_last { "└── " } else { "├── " };
        output.push_str(&format!("{}{}{}\n", prefix, marker, name));

        if path.is_dir() {
            output.push_str(&generate_tree_view(&path, excludes, depth + 1));
        }
    }
    output
}