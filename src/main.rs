use anyhow::{Context, Result};
use cargo_shape_check::{
    diff_manifests, hash_crate, hash_workspace, load_manifest, save_manifest, to_manifest,
    MANIFEST_FILENAME,
};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
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

    let workspace_root = find_workspace_root(args.manifest_path.as_deref())?;
    let action = args.action.unwrap_or(Action::Check);

    match action {
        Action::Check => cmd_check(&workspace_root, args.json, args.quiet),
        Action::Save => cmd_save(&workspace_root, args.json),
        Action::Hash {
            crate_path,
            debug_text,
        } => cmd_hash(&crate_path, debug_text),
        Action::Status => cmd_status(&workspace_root, args.json),
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
