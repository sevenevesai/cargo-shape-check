use anyhow::{Context, Result};
use cargo_shape_check::{
    crate_rel_paths, diff_manifests, hash_crate, hash_workspace, load_manifest, save_manifest,
    to_manifest, workspace_crate_map, MANIFEST_FILENAME,
};
use clap::{Parser, Subcommand};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process;

#[derive(Parser)]
#[command(
    name = "cargo-shape-check",
    bin_name = "cargo",
    version,
    about = "Skip unnecessary downstream crate rebuilds by hashing the public API surface"
)]
struct Cargo {
    #[command(subcommand)]
    command: CargoCmd,
}

#[derive(Subcommand)]
enum CargoCmd {
    ShapeCheck(Args),
}

#[derive(Parser)]
#[command(
    version,
    about = "Hash the public API surface of workspace crates and detect changes"
)]
struct Args {
    #[command(subcommand)]
    action: Option<Action>,

    /// Path to Cargo.toml
    #[arg(long, global = true)]
    manifest_path: Option<PathBuf>,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    /// Suppress unchanged crates in output
    #[arg(short, long, global = true)]
    quiet: bool,
}

#[derive(Subcommand)]
enum Action {
    /// Check which crates have public API changes since last save (default)
    Check,
    /// Save current hashes as baseline to .shape-check.json
    Save,
    /// Hash a single crate
    Hash {
        /// Path to the crate directory
        #[arg()]
        crate_path: PathBuf,
        /// Print the canonical public surface text
        #[arg(long)]
        debug_text: bool,
    },
    /// Show the dependency-graph impact of current changes
    Status,
    /// Build the workspace, skipping downstream rebuilds when public APIs are unchanged
    Build {
        /// Extra arguments passed to cargo build
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cargo_args: Vec<String>,
    },
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    let Cargo {
        command: CargoCmd::ShapeCheck(args),
    } = Cargo::parse();

    let action = args.action.unwrap_or(Action::Check);

    // `hash` operates on a single crate and does not need a workspace root
    if let Action::Hash {
        crate_path,
        debug_text,
    } = action
    {
        return cmd_hash(&crate_path, debug_text);
    }

    let workspace_root = find_workspace_root(args.manifest_path.as_deref())?;

    match action {
        Action::Check => cmd_check(&workspace_root, args.json, args.quiet),
        Action::Save => cmd_save(&workspace_root, args.json),
        Action::Hash { .. } => unreachable!(),
        Action::Status => cmd_status(&workspace_root, args.json),
        Action::Build { cargo_args } => cmd_build(&workspace_root, &cargo_args),
    }
}

fn cmd_check(workspace_root: &PathBuf, json: bool, quiet: bool) -> Result<()> {
    let shapes = hash_workspace(workspace_root)?;
    let current = to_manifest(&shapes);

    let saved = load_manifest(workspace_root)?;
    match saved {
        None => {
            if json {
                println!("{}", serde_json::to_string_pretty(&current)?);
            } else {
                println!(
                    "No baseline found. Run `cargo shape-check save` to create {}",
                    MANIFEST_FILENAME
                );
                println!();
                println!("Current public API hashes ({} crates):", current.crates.len());
                for (name, hash) in &current.crates {
                    println!("  {name:<40} {}", &hash[..16]);
                }
            }
        }
        Some(saved) => {
            let (unchanged, changed, added) = diff_manifests(&saved, &current);
            let removed: Vec<String> = saved
                .crates
                .keys()
                .filter(|k| !current.crates.contains_key(*k))
                .cloned()
                .collect();

            if json {
                let report = serde_json::json!({
                    "unchanged": unchanged,
                    "changed": changed,
                    "added": added,
                    "removed": removed,
                });
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                if changed.is_empty() && added.is_empty() && removed.is_empty() {
                    println!(
                        "No public API changes detected across {} crates. Downstream rebuilds can be skipped.",
                        unchanged.len()
                    );
                    return Ok(());
                }

                if !changed.is_empty() {
                    println!("Public API changed ({}):", changed.len());
                    for name in &changed {
                        let old = saved.crates.get(name).map(|h| &h[..16]).unwrap_or("???");
                        let new = current.crates.get(name).map(|h| &h[..16]).unwrap_or("???");
                        println!("  ~ {name:<40} {old} -> {new}");
                    }
                }
                if !added.is_empty() {
                    println!("New crates ({}):", added.len());
                    for name in &added {
                        println!("  + {name}");
                    }
                }
                if !removed.is_empty() {
                    println!("Removed crates ({}):", removed.len());
                    for name in &removed {
                        println!("  - {name}");
                    }
                }
                if !quiet && !unchanged.is_empty() {
                    println!("Unchanged ({}):", unchanged.len());
                    for name in &unchanged {
                        println!("    {name}");
                    }
                }
            }

            if !changed.is_empty() || !added.is_empty() || !removed.is_empty() {
                process::exit(1);
            }
        }
    }
    Ok(())
}

fn cmd_save(workspace_root: &PathBuf, json: bool) -> Result<()> {
    let shapes = hash_workspace(workspace_root)?;
    let manifest = to_manifest(&shapes);
    save_manifest(workspace_root, &manifest)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&manifest)?);
    } else {
        println!(
            "Saved {} crate hashes to {}",
            manifest.crates.len(),
            workspace_root.join(MANIFEST_FILENAME).display()
        );
    }
    Ok(())
}

fn cmd_hash(crate_path: &PathBuf, debug_text: bool) -> Result<()> {
    let shape = hash_crate(crate_path)?;
    if debug_text {
        println!("{}", shape.canonical_text);
        eprintln!("hash: {}", shape.hash);
    } else {
        println!("{}", shape.hash);
    }
    Ok(())
}

fn cmd_status(workspace_root: &PathBuf, json: bool) -> Result<()> {
    let shapes = hash_workspace(workspace_root)?;
    let current = to_manifest(&shapes);

    let saved = match load_manifest(workspace_root)? {
        Some(s) => s,
        None => {
            eprintln!(
                "No baseline. Run `cargo shape-check save` first."
            );
            process::exit(1);
        }
    };

    let (unchanged, changed, added) = diff_manifests(&saved, &current);
    let private_only = changed.is_empty() && added.is_empty();

    if json {
        let report = serde_json::json!({
            "private_only": private_only,
            "unchanged_count": unchanged.len(),
            "changed": changed,
            "added": added,
            "verdict": if private_only { "skip" } else { "rebuild" },
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if private_only {
        println!(
            "All {} crates have unchanged public APIs. Downstream rebuilds can be skipped.",
            unchanged.len()
        );
    } else {
        println!("Public API changes detected — downstream rebuild required:");
        for name in &changed {
            println!("  ~ {name} (changed)");
        }
        for name in &added {
            println!("  + {name} (new)");
        }
        println!(
            "\n{} unchanged, {} changed, {} new",
            unchanged.len(),
            changed.len(),
            added.len()
        );
    }

    if !private_only {
        process::exit(1);
    }
    Ok(())
}

fn cmd_build(workspace_root: &PathBuf, cargo_args: &[String]) -> Result<()> {
    let baseline = load_manifest(workspace_root)?;

    if baseline.is_none() {
        eprintln!("shape-check: no baseline found, running full build and saving baseline");
        run_cargo(workspace_root, &["build"], cargo_args)?;
        let shapes = hash_workspace(workspace_root)?;
        save_manifest(workspace_root, &to_manifest(&shapes))?;
        eprintln!(
            "shape-check: baseline saved ({} crates)",
            shapes.len()
        );
        return Ok(());
    }
    let baseline = baseline.unwrap();

    let changed_crates = find_changed_crates(workspace_root)?;
    if changed_crates.is_empty() {
        run_cargo(workspace_root, &["build"], cargo_args)?;
        return Ok(());
    }

    let crate_map = workspace_crate_map(workspace_root)?;
    let total_crates = crate_map.len();

    let mut public_changes: Vec<String> = Vec::new();
    let mut private_changes: Vec<String> = Vec::new();

    for name in &changed_crates {
        if let Some(path) = crate_map.get(name) {
            match hash_crate(path) {
                Ok(shape) => {
                    if baseline.crates.get(name) != Some(&shape.hash) {
                        public_changes.push(name.clone());
                    } else {
                        private_changes.push(name.clone());
                    }
                }
                Err(_) => {
                    public_changes.push(name.clone());
                }
            }
        }
    }

    if !public_changes.is_empty() {
        eprintln!(
            "shape-check: public API changed in [{}], full rebuild",
            public_changes.join(", ")
        );
        run_cargo(workspace_root, &["build"], cargo_args)?;
        let shapes = hash_workspace(workspace_root)?;
        save_manifest(workspace_root, &to_manifest(&shapes))?;
        return Ok(());
    }

    // Private changes only. Build just the changed crates, skip their dependents.
    let pkg_args: Vec<String> = changed_crates
        .iter()
        .flat_map(|name| vec!["-p".to_string(), name.clone()])
        .collect();

    let mut all_args = pkg_args.iter().map(|s| s.as_str()).collect::<Vec<_>>();
    let extra: Vec<&str> = cargo_args.iter().map(|s| s.as_str()).collect();
    all_args.extend(&extra);

    let build_args: Vec<&str> = std::iter::once("build").chain(all_args).collect();
    run_cargo_raw(workspace_root, &build_args)?;

    let skipped = total_crates - changed_crates.len();
    eprintln!(
        "shape-check: private changes only in [{}], {} downstream crates skipped",
        changed_crates.iter().cloned().collect::<Vec<_>>().join(", "),
        skipped
    );
    Ok(())
}

fn find_changed_crates(workspace_root: &Path) -> Result<BTreeSet<String>> {
    let rel_paths = crate_rel_paths(workspace_root)?;

    // Collect all changed files from git
    let mut changed_files: Vec<String> = Vec::new();

    // Unstaged changes
    if let Ok(output) = std::process::Command::new("git")
        .args(["diff", "--name-only"])
        .current_dir(workspace_root)
        .output()
    {
        if output.status.success() {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    changed_files.push(trimmed.to_string());
                }
            }
        }
    }

    // Staged changes
    if let Ok(output) = std::process::Command::new("git")
        .args(["diff", "--name-only", "--cached"])
        .current_dir(workspace_root)
        .output()
    {
        if output.status.success() {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    changed_files.push(trimmed.to_string());
                }
            }
        }
    }

    // Untracked files
    if let Ok(output) = std::process::Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(workspace_root)
        .output()
    {
        if output.status.success() {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    changed_files.push(trimmed.to_string());
                }
            }
        }
    }

    // Map files to crates
    let mut result = BTreeSet::new();
    for file in &changed_files {
        let normalized = file.replace('\\', "/");
        if !normalized.ends_with(".rs") && !normalized.ends_with("Cargo.toml") {
            continue;
        }
        for (name, rel) in &rel_paths {
            if normalized.starts_with(&format!("{}/", rel)) {
                result.insert(name.clone());
                break;
            }
        }
    }

    Ok(result)
}

fn run_cargo(workspace_root: &Path, command: &[&str], extra_args: &[String]) -> Result<()> {
    let extra: Vec<&str> = extra_args.iter().map(|s| s.as_str()).collect();
    let all: Vec<&str> = command.iter().copied().chain(extra).collect();
    run_cargo_raw(workspace_root, &all)
}

fn run_cargo_raw(workspace_root: &Path, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new("cargo")
        .args(args)
        .current_dir(workspace_root)
        .status()
        .context("failed to run cargo")?;

    if !status.success() {
        process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn find_workspace_root(manifest_path: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(path) = manifest_path {
        let path = path.canonicalize().context("canonicalize manifest path")?;
        return Ok(path
            .parent()
            .context("manifest path has no parent")?
            .to_path_buf());
    }

    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join("Cargo.toml").exists() {
            let text = std::fs::read_to_string(dir.join("Cargo.toml"))?;
            if text.contains("[workspace]") {
                return Ok(dir);
            }
        }
        if !dir.pop() {
            break;
        }
    }
    anyhow::bail!("could not find workspace Cargo.toml in any parent directory")
}
