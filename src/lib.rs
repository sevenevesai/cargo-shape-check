use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ShapeManifest {
    pub crates: BTreeMap<String, String>,
    pub version: String,
}

#[derive(Debug)]
pub struct CrateShape {
    pub name: String,
    pub path: PathBuf,
    pub canonical_text: String,
    pub hash: String,
}

pub fn hash_workspace(workspace_root: &Path) -> Result<Vec<CrateShape>> {
    let crate_paths = resolve_workspace_members(workspace_root)?;
    let mut results = Vec::new();
    for crate_path in crate_paths {
        match hash_crate(&crate_path) {
            Ok(shape) => results.push(shape),
            Err(e) => {
                eprintln!("warn: {}: {}", crate_path.display(), e);
            }
        }
    }
    results.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(results)
}

pub fn hash_crate(crate_path: &Path) -> Result<CrateShape> {
    let name = crate_name(crate_path)?;
    let entry = find_crate_entry(crate_path)?;
    if !entry.exists() {
        anyhow::bail!("entry point {} does not exist", entry.display());
    }

    let mut canonical = String::new();
    walk_module(&entry, "", true, &mut canonical)?;

    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let hash = hex_encode(hasher.finalize());

    Ok(CrateShape {
        name,
        path: crate_path.to_path_buf(),
        canonical_text: canonical,
        hash,
    })
}

pub fn to_manifest(shapes: &[CrateShape]) -> ShapeManifest {
    let mut crates = BTreeMap::new();
    for s in shapes {
        crates.insert(s.name.clone(), s.hash.clone());
    }
    ShapeManifest {
        crates,
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

pub fn diff_manifests(
    old: &ShapeManifest,
    new: &ShapeManifest,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut unchanged = Vec::new();
    let mut changed = Vec::new();
    let mut added = Vec::new();

    for (name, new_hash) in &new.crates {
        match old.crates.get(name) {
            Some(old_hash) if old_hash == new_hash => unchanged.push(name.clone()),
            Some(_) => changed.push(name.clone()),
            None => added.push(name.clone()),
        }
    }
    (unchanged, changed, added)
}

pub fn workspace_crate_map(workspace_root: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let members = resolve_workspace_members(workspace_root)?;
    let mut map = BTreeMap::new();
    for crate_path in members {
        if let Ok(name) = crate_name(&crate_path) {
            map.insert(name, crate_path);
        }
    }
    Ok(map)
}

pub fn crate_rel_paths(workspace_root: &Path) -> Result<BTreeMap<String, String>> {
    let members = resolve_workspace_members(workspace_root)?;
    let mut map = BTreeMap::new();
    for crate_path in members {
        if let Ok(name) = crate_name(&crate_path) {
            let rel = crate_path
                .strip_prefix(workspace_root)
                .unwrap_or(&crate_path)
                .to_string_lossy()
                .replace('\\', "/");
            map.insert(name, rel);
        }
    }
    Ok(map)
}

pub const MANIFEST_FILENAME: &str = ".shape-check.json";

pub fn load_manifest(workspace_root: &Path) -> Result<Option<ShapeManifest>> {
    let path = workspace_root.join(MANIFEST_FILENAME);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    let manifest: ShapeManifest = serde_json::from_str(&text)?;
    Ok(Some(manifest))
}

pub fn save_manifest(workspace_root: &Path, manifest: &ShapeManifest) -> Result<()> {
    let path = workspace_root.join(MANIFEST_FILENAME);
    let text = serde_json::to_string_pretty(manifest)?;
    fs::write(&path, text)?;
    Ok(())
}

// ============================================================================
// Workspace parsing
// ============================================================================

pub fn resolve_workspace_members(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let cargo_toml = workspace_root.join("Cargo.toml");
    let toml_text =
        fs::read_to_string(&cargo_toml).with_context(|| format!("read {}", cargo_toml.display()))?;
    let manifest: toml::Value = toml::from_str(&toml_text)?;

    let workspace = manifest
        .get("workspace")
        .context("no [workspace] section in Cargo.toml")?
        .as_table()
        .context("[workspace] is not a table")?;

    let members = workspace
        .get("members")
        .and_then(|v| v.as_array())
        .context("no workspace.members")?;

    let excludes: std::collections::HashSet<String> = workspace
        .get("exclude")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut crate_paths: Vec<PathBuf> = Vec::new();
    for m in members {
        let glob_str = m.as_str().unwrap_or("").trim_end_matches('/');
        if glob_str.is_empty() {
            continue;
        }
        if glob_str.contains('*') {
            let parent_glob = glob_str.replace("/*", "").replace("*", "");
            let parent = workspace_root.join(&parent_glob);
            if parent.is_dir() {
                let mut entries: Vec<_> = fs::read_dir(&parent)?
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
                    .collect();
                entries.sort_by_key(|e| e.file_name());
                for entry in entries {
                    let p = entry.path();
                    if p.join("Cargo.toml").exists() {
                        let rel = p
                            .strip_prefix(workspace_root)
                            .unwrap_or(&p)
                            .to_string_lossy()
                            .replace('\\', "/");
                        if !excludes.contains(&rel) {
                            crate_paths.push(p);
                        }
                    }
                }
            }
        } else {
            let p = workspace_root.join(glob_str);
            if p.join("Cargo.toml").exists() {
                let rel = p
                    .strip_prefix(workspace_root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/");
                if !excludes.contains(&rel) {
                    crate_paths.push(p);
                }
            }
        }
    }

    Ok(crate_paths)
}

fn crate_name(crate_path: &Path) -> Result<String> {
    let toml_text = fs::read_to_string(crate_path.join("Cargo.toml"))?;
    let v: toml::Value = toml::from_str(&toml_text)?;
    let name = v
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .context("no package.name in Cargo.toml")?
        .to_string();
    Ok(name)
}

fn find_crate_entry(crate_path: &Path) -> Result<PathBuf> {
    let cargo_toml = crate_path.join("Cargo.toml");
    if cargo_toml.exists() {
        let toml_text = fs::read_to_string(&cargo_toml)?;
        let v: toml::Value = toml::from_str(&toml_text)?;
        if let Some(lib) = v.get("lib").and_then(|l| l.get("path")).and_then(|p| p.as_str()) {
            return Ok(crate_path.join(lib));
        }
        if let Some(bins) = v.get("bin").and_then(|b| b.as_array())
            && let Some(first) = bins.first()
            && let Some(p) = first.get("path").and_then(|p| p.as_str())
        {
            return Ok(crate_path.join(p));
        }
    }
    let candidates = [
        crate_path.join("src/lib.rs"),
        crate_path.join("src/main.rs"),
        crate_path.join("lib.rs"),
        crate_path.join("main.rs"),
    ];
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    anyhow::bail!("no entry point found for crate at {}", crate_path.display())
}

// ============================================================================
// Module walking + public surface extraction
// ============================================================================

fn walk_module(file_path: &Path, mod_path: &str, is_crate_root: bool, out: &mut String) -> Result<()> {
    let src = fs::read_to_string(file_path).with_context(|| format!("parse {}", file_path.display()))?;
    let file = syn::parse_file(&src).with_context(|| format!("parse {}", file_path.display()))?;

    let file_stem = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    // Crate entry points resolve submodules as siblings regardless of filename
    let effective_stem = if is_crate_root { "lib".to_string() } else { file_stem.clone() };

    let mut visitor = ShapeVisitor {
        items: Vec::new(),
        mod_path: mod_path.to_string(),
        file_dir: file_path.parent().map(Path::to_path_buf).unwrap_or_default(),
        file_stem: effective_stem,
    };
    visitor.visit_file(&file);

    let mut items = visitor.items;
    items.sort();

    out.push_str(&format!("=== mod {} ===\n", mod_path));
    for item_text in &items {
        out.push_str(item_text);
        out.push('\n');
    }

    // Collect which private modules have items re-exported via `pub use`
    let reexport_targets = find_reexport_targets(&file);

    for sub in find_submodules(&file) {
        let sub_path = resolve_submodule(&visitor.file_dir, &visitor.file_stem, &sub.name)?;
        let new_mod_path = if mod_path.is_empty() {
            sub.name.clone()
        } else {
            format!("{}::{}", mod_path, sub.name)
        };
        if sub.is_pub || reexport_targets.contains(&sub.name) {
            walk_module(&sub_path, &new_mod_path, false, out)?;
        }
    }

    Ok(())
}

/// Find private module names that are targets of `pub use` re-exports.
///
/// Matches patterns like:
///   pub use foo::Bar;
///   pub use foo::*;
///   pub use foo::{A, B};
fn find_reexport_targets(file: &syn::File) -> std::collections::HashSet<String> {
    let mut targets = std::collections::HashSet::new();
    for item in &file.items {
        if let syn::Item::Use(u) = item {
            if !matches!(u.vis, syn::Visibility::Public(_)) {
                continue;
            }
            collect_use_roots(&u.tree, &mut targets);
        }
    }
    targets
}

fn collect_use_roots(tree: &syn::UseTree, targets: &mut std::collections::HashSet<String>) {
    match tree {
        syn::UseTree::Path(p) => {
            let name = p.ident.to_string();
            let name = name.strip_prefix("r#").unwrap_or(&name).to_string();
            // Only the first path segment is a local module name
            targets.insert(name);
        }
        syn::UseTree::Group(g) => {
            for item in &g.items {
                collect_use_roots(item, targets);
            }
        }
        // `pub use SomeItem;` or `pub use *;` at root level -- no module prefix
        syn::UseTree::Name(_) | syn::UseTree::Rename(_) | syn::UseTree::Glob(_) => {}
    }
}

struct SubmoduleDecl {
    name: String,
    is_pub: bool,
}

fn find_submodules(file: &syn::File) -> Vec<SubmoduleDecl> {
    let mut subs = Vec::new();
    for item in &file.items {
        if let syn::Item::Mod(m) = item
            && m.content.is_none()
        {
            let is_pub = matches!(m.vis, syn::Visibility::Public(_));
            let name = m.ident.to_string();
            let name = name.strip_prefix("r#").unwrap_or(&name).to_string();
            subs.push(SubmoduleDecl { name, is_pub });
        }
    }
    subs
}

fn resolve_submodule(parent_dir: &Path, file_stem: &str, name: &str) -> Result<PathBuf> {
    let candidates: Vec<PathBuf> = if file_stem == "lib" || file_stem == "mod" || file_stem == "main"
    {
        vec![
            parent_dir.join(format!("{}.rs", name)),
            parent_dir.join(name).join("mod.rs"),
        ]
    } else {
        vec![
            parent_dir.join(file_stem).join(format!("{}.rs", name)),
            parent_dir.join(file_stem).join(name).join("mod.rs"),
        ]
    };

    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    anyhow::bail!(
        "could not resolve submodule `{}` from `{}/{}.rs`",
        name,
        parent_dir.display(),
        file_stem,
    )
}

// ============================================================================
// syn visitor — extracts public surface items
// ============================================================================

struct ShapeVisitor {
    items: Vec<String>,
    mod_path: String,
    file_dir: PathBuf,
    file_stem: String,
}

impl ShapeVisitor {
    fn is_visible(&self, vis: &syn::Visibility) -> bool {
        matches!(vis, syn::Visibility::Public(_))
    }
}

impl<'ast> Visit<'ast> for ShapeVisitor {
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        if !self.is_visible(&f.vis) {
            return;
        }
        let inline = has_inline_attr(&f.attrs);
        let generic = has_type_or_const_generics(&f.sig.generics);
        let constfn = f.sig.constness.is_some();
        let body_visible_downstream = inline || generic || constfn;
        let body = if body_visible_downstream {
            quote::quote!(#f).to_string()
        } else {
            String::new()
        };
        let text = format!(
            "fn {} {} {}{}{}",
            sig_text(&f.sig),
            relevant_attrs(&f.attrs),
            if body_visible_downstream {
                "<<BODY: "
            } else {
                ""
            },
            body,
            if body_visible_downstream { ">>" } else { "" }
        );
        self.items.push(format!("[fn] {}::{}", self.mod_path, text));
    }

    fn visit_item_struct(&mut self, s: &'ast syn::ItemStruct) {
        if !self.is_visible(&s.vis) {
            return;
        }
        let text = quote::quote!(#s).to_string();
        self.items
            .push(format!("[struct] {}::{}", self.mod_path, text));
    }

    fn visit_item_enum(&mut self, e: &'ast syn::ItemEnum) {
        if !self.is_visible(&e.vis) {
            return;
        }
        let text = quote::quote!(#e).to_string();
        self.items.push(format!("[enum] {}::{}", self.mod_path, text));
    }

    fn visit_item_trait(&mut self, t: &'ast syn::ItemTrait) {
        if !self.is_visible(&t.vis) {
            return;
        }
        let text = quote::quote!(#t).to_string();
        self.items
            .push(format!("[trait] {}::{}", self.mod_path, text));
    }

    fn visit_item_type(&mut self, t: &'ast syn::ItemType) {
        if !self.is_visible(&t.vis) {
            return;
        }
        let text = quote::quote!(#t).to_string();
        self.items
            .push(format!("[type] {}::{}", self.mod_path, text));
    }

    fn visit_item_const(&mut self, c: &'ast syn::ItemConst) {
        if !self.is_visible(&c.vis) {
            return;
        }
        let text = quote::quote!(#c).to_string();
        self.items.push(format!("[const] {}::{}", self.mod_path, text));
    }

    fn visit_item_static(&mut self, s: &'ast syn::ItemStatic) {
        if !self.is_visible(&s.vis) {
            return;
        }
        let text = quote::quote!(#s).to_string();
        self.items
            .push(format!("[static] {}::{}", self.mod_path, text));
    }

    fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
        if !self.is_visible(&u.vis) {
            return;
        }
        let text = quote::quote!(#u).to_string();
        self.items.push(format!("[use] {}::{}", self.mod_path, text));
    }

    fn visit_item_macro(&mut self, m: &'ast syn::ItemMacro) {
        let exported = m.attrs.iter().any(|a| a.path().is_ident("macro_export"));
        if !exported {
            return;
        }
        let text = quote::quote!(#m).to_string();
        self.items
            .push(format!("[macro] {}::{}", self.mod_path, text));
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let mut buf = String::from("impl");
        let generics = &i.generics;
        if !i.generics.params.is_empty() {
            buf.push_str(&quote::quote!(#generics).to_string());
        }
        let self_ty = &i.self_ty;
        if let Some((_, trait_path, _)) = &i.trait_ {
            buf.push(' ');
            buf.push_str(&quote::quote!(#trait_path).to_string());
            buf.push_str(" for ");
        } else {
            buf.push(' ');
        }
        buf.push_str(&quote::quote!(#self_ty).to_string());

        let is_trait_impl = i.trait_.is_some();
        for item in &i.items {
            match item {
                syn::ImplItem::Fn(f) => {
                    let visible = is_trait_impl || matches!(f.vis, syn::Visibility::Public(_));
                    if !visible {
                        continue;
                    }
                    let inline = has_inline_attr(&f.attrs);
                    let generic = has_type_or_const_generics(&f.sig.generics);
                    let constfn = f.sig.constness.is_some();
                    let body_visible = inline || generic || constfn;
                    let sig = sig_text(&f.sig);
                    if body_visible {
                        let body = quote::quote!(#f).to_string();
                        buf.push_str(&format!("\n  fn {} <<BODY: {}>>", sig, body));
                    } else {
                        buf.push_str(&format!("\n  fn {};", sig));
                    }
                }
                syn::ImplItem::Const(c) => {
                    let visible = is_trait_impl || matches!(c.vis, syn::Visibility::Public(_));
                    if !visible {
                        continue;
                    }
                    buf.push_str(&format!("\n  {}", quote::quote!(#c)));
                }
                syn::ImplItem::Type(t) => {
                    let visible = is_trait_impl || matches!(t.vis, syn::Visibility::Public(_));
                    if !visible {
                        continue;
                    }
                    buf.push_str(&format!("\n  {}", quote::quote!(#t)));
                }
                _ => {}
            }
        }

        self.items
            .push(format!("[impl] {}::{}", self.mod_path, buf));
    }

    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if m.content.is_none() {
            return;
        }
        let visible = matches!(m.vis, syn::Visibility::Public(_));
        if !visible {
            return;
        }
        let new_mod_path = if self.mod_path.is_empty() {
            m.ident.to_string()
        } else {
            format!("{}::{}", self.mod_path, m.ident)
        };
        let mut sub_visitor = ShapeVisitor {
            items: Vec::new(),
            mod_path: new_mod_path.clone(),
            file_dir: self.file_dir.clone(),
            file_stem: self.file_stem.clone(),
        };
        if let Some((_, items)) = &m.content {
            for item in items {
                sub_visitor.visit_item(item);
            }
        }
        sub_visitor.items.sort();
        self.items.push(format!(
            "[mod] {}::{} {{\n{}\n}}",
            self.mod_path,
            m.ident,
            sub_visitor.items.join("\n")
        ));
    }
}

fn sig_text(sig: &syn::Signature) -> String {
    quote::quote!(#sig).to_string()
}

fn has_inline_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| a.path().is_ident("inline"))
}

fn has_type_or_const_generics(g: &syn::Generics) -> bool {
    g.params.iter().any(|p| {
        matches!(
            p,
            syn::GenericParam::Type(_) | syn::GenericParam::Const(_)
        )
    })
}

fn relevant_attrs(attrs: &[syn::Attribute]) -> String {
    let mut buf = String::new();
    for a in attrs {
        let path = quote::quote!(#a).to_string();
        if !path.contains("# [doc") && !path.contains("#[doc") {
            buf.push_str(&path);
            buf.push(' ');
        }
    }
    buf
}

fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}
