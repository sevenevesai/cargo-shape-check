# Public API shape hashing for Cargo workspaces

[<img alt="github" src="https://img.shields.io/badge/github-sevenevesai/cargo--shape--check-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20">](https://github.com/sevenevesai/cargo-shape-check)
[<img alt="crates.io" src="https://img.shields.io/crates/v/cargo-shape-check.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20">](https://crates.io/crates/cargo-shape-check)

Cargo rebuilds every downstream dependent when any source file in an upstream
crate changes, even if the change is purely internal. `cargo-shape-check`
hashes only the public API surface of each crate in a workspace and reports
which crates actually changed their public interface.

In controlled experiments on rust-analyzer, skipping unnecessary downstream
rebuilds delivers a **35x speedup** on private edits (17s to 0.5s) with zero
false-skips across 25 test cases.

<br>

## Install

```
cargo install cargo-shape-check
```

<br>

## Usage

Save the current public API hashes as a baseline, make some changes, then check
what changed.

```console
$ cargo shape-check save
Saved 44 crate hashes to .shape-check.json

$ cargo shape-check status
All 44 crates have unchanged public APIs. Downstream rebuilds can be skipped.

$ cargo shape-check check --quiet
Public API changed (1):
  ~ stdx  544756717d56c90c -> 0fd2bae739a50167
```

The `status` command exits 0 when all public APIs are unchanged and 1 when any
crate has a public surface change. The `check` command shows a full diff against
the saved baseline.

```console
$ cargo shape-check hash path/to/crate
a3f2b8c9d1e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0

$ cargo shape-check check --json
{
  "unchanged": ["hir", "hir-def", ...],
  "changed": ["stdx"],
  "added": [],
  "removed": []
}
```

<br>

## Motivation

Cargo uses file mtimes (or optionally content checksums) to decide when to
rebuild. When a source file in a leaf crate changes, all transitive dependents
are rebuilt, regardless of whether the public API actually changed.

In large Rust workspaces this creates significant wasted work. We measured four
major open source projects by walking 200 recent commits each, hashing the
public API at each commit, and counting how many downstream rebuilds were
triggered by changes that did not alter the public surface.

| Project | Crates | Private-only changes | Wasted downstream rebuilds |
|---|---|---|---|
| rust-analyzer | 44 | 75% | 66% |
| Bevy | 78 | 73% | 70% |
| Nushell | 38 | 95% | 93% |
| Deno | 73 | 78% | 81% |

73 to 95% of crate-level source changes across these projects do not touch the
public API. Cargo rebuilds downstream anyway. See
[rust-lang/cargo#14604](https://github.com/rust-lang/cargo/issues/14604) for
upstream discussion of this problem.

<br>

## What gets hashed

The public surface of a crate consists of all items visible to downstream
dependents. Specifically:

- **Functions** `pub fn` signatures including return types, parameters,
  generics, and bounds. Bodies are excluded unless downstream crates can see
  them (see below).

- **Data types** `pub struct`, `pub enum` definitions including all fields,
  variants, and repr attributes.

- **Traits** `pub trait` definitions including default method bodies, which
  downstream crates use directly.

- **Other items** `pub type` aliases, `pub const`, `pub static`, `pub use`
  re-exports, and `#[macro_export]` macros.

- **Impl blocks** Public methods and trait implementations, including the impl
  header and associated types/consts.

Function bodies are included in the hash when downstream crates can observe
them:

- `#[inline]` and `#[inline(always)]` functions, whose bodies are inlined into
  call sites
- Generic functions with type or const parameters, which downstream crates
  monomorphize
- `const fn`, whose bodies downstream crates may const-evaluate

All other `pub fn` bodies are excluded from the hash. A comment change, a local
variable rename, or a refactor inside a non-inline non-generic function body
will not change the hash.

<br>

## Known limitations

- **`pub use private_mod::Item`** re-exports from private modules are not
  resolved. If the re-exported item changes in a way not visible from the
  `pub use` declaration, this may produce a false skip.

- **Macro-generated public surfaces** are not detected. The tool parses source
  text, not macro-expanded output.

- **Auto-trait inference** changes (`Send`/`Sync` becoming `!Send`/`!Sync` due
  to a private field type change) are not detected from source-level parsing.

- **`pub fn() -> impl Trait`** where the inferred concrete type changes without
  the source signature changing.

These are inherent to a source-level approach. A rustc-internal implementation
operating on post-expansion, type-resolved data would close these gaps.

No false-skips were observed across 25 adversarial test cases covering comments,
local variable renames, private function additions, doc-only changes, generic
function body edits, inline function body edits, new public items, visibility
changes, and trait method additions.

<br>

## How it works

1. Parses each crate's source tree with [syn](https://crates.io/crates/syn)
2. Extracts publicly-visible items using a `syn::visit::Visit` traversal
3. Canonicalizes the output (sorted, formatted via [quote](https://crates.io/crates/quote))
4. Hashes with SHA-256
5. Compares against a saved baseline to detect changes

No compilation required. No nightly toolchain. Hashing runs in ~25ms per crate.

<br>

#### License

<sup>
Licensed under either of <a href="LICENSE-APACHE">Apache License, Version
2.0</a> or <a href="LICENSE-MIT">MIT license</a> at your option.
</sup>

<br>

<sub>
Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
</sub>
