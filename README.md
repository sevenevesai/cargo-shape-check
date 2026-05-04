# Public API shape hashing for Cargo workspaces

[<img alt="github" src="https://img.shields.io/badge/github-sevenevesai/cargo--shape--check-8da0cb?style=for-the-badge&labelColor=555555&logo=github" height="20">](https://github.com/sevenevesai/cargo-shape-check)
[<img alt="crates.io" src="https://img.shields.io/crates/v/cargo-shape-check.svg?style=for-the-badge&color=fc8d62&logo=rust" height="20">](https://crates.io/crates/cargo-shape-check)

Cargo rebuilds every downstream dependent when any source file in an upstream
crate changes, even if the change is purely internal. `cargo-shape-check`
hashes only the public API surface of each crate in a workspace and tells you
which crates actually changed their public interface.

<br>

## Install

```
cargo install cargo-shape-check
```

<br>

## Usage

The diagnostic commands tell you whether changes are private-only or touch the
public API:

```console
$ cargo shape-check save
Saved 44 crate hashes to .shape-check.json

$ echo "// private comment" >> crates/stdx/src/lib.rs

$ cargo shape-check check --quiet
No public API changes detected across 44 crates. Downstream rebuilds can be skipped.

$ cargo shape-check status --json
{
  "private_only": true,
  "unchanged_count": 44,
  "changed": [],
  "added": [],
  "verdict": "skip"
}
```

`check` diffs the current state against the saved baseline and exits 1 if any
public API changed. `status` gives a summary verdict. `hash` prints the hash of
a single crate:

```console
$ cargo shape-check hash crates/stdx
a1b2c3d4e5f6...
```

### Build command

The `build` subcommand wraps `cargo build` with `-p` scoping. It uses git to
find which crates have source changes, hashes them, and passes only the changed
crates to `cargo build -p` when their public API is unchanged. Any user-supplied
arguments (including `-p` targets) are forwarded to cargo.

```console
$ cargo shape-check build
shape-check: no baseline found, running full build and saving baseline
shape-check: baseline saved (44 crates)

$ echo "// private comment" >> crates/stdx/src/lib.rs

$ cargo shape-check build
shape-check: private-only changes in [stdx], rebuilding changed crates only
```

**Important:** The `-p` scoping means the build command only rebuilds the
changed crates themselves. It does not rebuild their downstream dependents or
re-link workspace binaries. This is fast (~0.5s vs ~17s for a full workspace
rebuild on rust-analyzer) but it is not the same build graph as `cargo build`.
Cargo does not retain this analysis between invocations, so the next `cargo
build` (or IDE background check) will rebuild dependents normally.

Fully skipping unnecessary downstream rebuilds requires native support inside
cargo's fingerprinting. See
[rust-lang/cargo#14604](https://github.com/rust-lang/cargo/issues/14604) for
the upstream proposal.

<br>

## Motivation

Cargo uses file mtimes (or optionally content checksums) to decide when to
rebuild. When a source file in a leaf crate changes, all transitive dependents
are rebuilt, regardless of whether the public API actually changed.

In large Rust workspaces this creates significant wasted work. We measured five
major open source projects by walking ~200 recent commits each, hashing the
public API at each commit, and counting how many downstream rebuilds were
triggered by changes that did not alter the public surface.

| Project | Crates | Private-only changes | Wasted downstream rebuilds |
|---|---|---|---|
| Zed | 239 | 66% | 51% |
| rust-analyzer | 44 | 75% | 66% |
| Bevy | 78 | 73% | 70% |
| Nushell | 38 | 95% | 93% |
| Deno | 73 | 78% | 81% |

66 to 95% of crate-level source changes across these projects do not touch the
public API. Cargo rebuilds downstream anyway. See
[rust-lang/cargo#14604](https://github.com/rust-lang/cargo/issues/14604) for
upstream discussion of this problem.

<br>

## Measurements

The speedup numbers were measured on rust-analyzer
(`d8e48581c354d482e8edd5e1c529d3200c92abc0`) with the `stdx` crate (22
in-workspace dependents).

**What was compared:**
- **Baseline:** `cargo build --quiet` (full workspace) after applying a private
  edit to stdx. ~17s median.
- **Scoped build:** `cargo build --quiet -p stdx` + shape hash check (~0.5s
  median) after the same edit.

This measures the time to rebuild the changed crate in isolation vs rebuilding
the full workspace. The scoped build does not rebuild dependents or re-link
workspace binaries.

25 adversarial test cases (comments, local renames, doc changes, generic body
edits, inline body edits, new pub items, visibility changes, trait method
additions) all classified correctly with 0 false-skips.

**Repro steps** (rust-analyzer):

```bash
# Clone and build rust-analyzer
git clone https://github.com/rust-lang/rust-analyzer
cd rust-analyzer
git checkout d8e48581c354d482e8edd5e1c529d3200c92abc0
rustup override set nightly
cargo build --quiet

# Install and save baseline
cargo install cargo-shape-check
cargo shape-check save

# Baseline: full workspace rebuild after private edit
echo "// private comment" >> crates/stdx/src/lib.rs
time cargo build --quiet
# ~17s (rebuilds stdx + 22 dependents)

git checkout -- crates/stdx/src/lib.rs
cargo build --quiet  # settle target/

# Scoped: rebuild only the changed crate
echo "// private comment" >> crates/stdx/src/lib.rs
time cargo build --quiet -p stdx
# ~0.4s (rebuilds stdx only)

# Verify the hash is unchanged
cargo shape-check check --quiet
# No public API changes detected
```

The 35x ratio (17s / 0.5s) represents the difference between rebuilding the
full workspace and rebuilding only the changed crate. It is the speedup ceiling
for what native cargo support (cargo#14604) could deliver by skipping
unnecessary downstream rebuilds.

Hardware: AMD Ryzen 9 7900, 64 GB RAM, Windows 11, SSD. Full methodology and
raw data in the
[research repo](https://github.com/sevenevesai/cargo-shape-check/issues/1#issuecomment-4372023169).

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

- **Macro-generated public surfaces** are not detected. The tool parses source
  text, not macro-expanded output.

- **Auto-trait inference** changes (`Send`/`Sync` becoming `!Send`/`!Sync` due
  to a private field type change) are not detected from source-level parsing.

- **`pub fn() -> impl Trait`** where the inferred concrete type changes without
  the source signature changing.

- **Implicit `#[inline]`** — the compiler may inline functions not explicitly
  marked, which source-level parsing cannot detect.

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
