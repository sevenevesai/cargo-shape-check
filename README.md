# cargo-shape-check

Skip unnecessary downstream crate rebuilds by hashing only the public API surface.

Cargo rebuilds every downstream dependent when any source file in an upstream crate changes — even if the change is purely internal (a comment, a private function body, a local variable rename). `cargo-shape-check` detects when a crate's *public API* is unchanged and reports that downstream rebuilds can be safely skipped.

## The problem

In a Cargo workspace with many internal crates, editing a high-fanin leaf crate triggers a rebuild cascade across all dependents. Our measurements across four major Rust projects show:

| Project | Crates | Private-only changes | Wasted downstream rebuilds |
|---|---|---|---|
| rust-analyzer | 44 | 75% | 66% |
| Bevy | 78 | 73% | 70% |
| Nushell | 38 | 95% | 93% |
| Deno | 73 | 78% | 81% |

**73–95% of crate-level source changes don't touch the public API.** Cargo rebuilds downstream anyway.

In controlled experiments on rust-analyzer, skipping unnecessary downstream rebuilds delivers a **35× speedup** on private edits (17s → 0.5s) with zero false-skips across 25 test cases.

## Install

```
cargo install cargo-shape-check
```

Or from source:

```
git clone https://github.com/sevenevesai/cargo-shape-check
cd cargo-shape-check
cargo install --path .
```

## Usage

```bash
# Save the current public API hashes as a baseline
cargo shape-check save

# Make some changes, then check what changed
cargo shape-check check

# Quick verdict: skip or rebuild?
cargo shape-check status

# Hash a single crate
cargo shape-check hash path/to/crate

# JSON output for scripting
cargo shape-check check --json
cargo shape-check status --json
```

### Typical workflow

```bash
# At the start of a work session (or in CI after checkout)
cargo shape-check save

# After editing code
cargo shape-check status
# "All 44 crates have unchanged public APIs. Downstream rebuilds can be skipped."
# → only rebuild the leaf crate you edited

# Or if you changed a public signature:
# "Public API changes detected — downstream rebuild required"
# → run the full cargo build
```

### CI integration

```yaml
- name: Check for unnecessary rebuilds
  run: |
    cargo shape-check save
    # ... run your changes ...
    cargo shape-check status --json | jq '.verdict'
```

## What gets hashed

The public API surface includes:
- `pub fn` signatures (return types, parameters, generics, bounds)
- `pub struct` / `pub enum` definitions (all fields, variants, `#[repr]`)
- `pub trait` definitions (including default method bodies)
- `pub type` aliases, `pub const`, `pub static`
- `pub use` re-exports
- `#[macro_export]` macros
- `impl` blocks (public methods and trait implementations)

Bodies of `pub fn` are included in the hash when downstream crates can see them:
- `#[inline]` / `#[inline(always)]` functions
- Generic functions (type or const params — downstream monomorphizes)
- `const fn` (downstream may const-evaluate)

Bodies of non-inline, non-generic, non-const `pub fn` are **excluded** — changes to these are invisible to downstream crates.

## What is NOT hashed (known limitations)

- **`pub use private_mod::Item` re-exports**: we don't resolve the re-export target from private modules
- **Macro-generated public surfaces**: we parse source text, not expanded macros
- **Auto-trait inference** (`Send`/`Sync`): a private field type change can alter auto-trait impls without changing the parsed source
- **`pub fn() -> impl Trait`**: the inferred concrete type is not visible in source

These are documented soundness gaps. For the vast majority of edits (75%+ as measured), the hash correctly identifies private-only changes. False-rebuilds (hash changes when it shouldn't) are safe; false-skips are the risk, and none were observed in 25 test cases.

## How it works

1. Parses each crate's source tree with [`syn`](https://crates.io/crates/syn)
2. Extracts all publicly-visible items using a `Visit` traversal
3. Canonicalizes the output (sorted, formatted via `quote`)
4. Hashes with SHA-256
5. Compares against saved baseline to detect changes

No compilation required. No nightly toolchain. Runs in ~25ms per crate.

## Background

This tool validates the hypothesis behind [rust-lang/cargo#14604](https://github.com/rust-lang/cargo/issues/14604): that interface-shape hashing can eliminate unnecessary downstream rebuilds. TypeScript has done this since 2019 (via d.ts text hashing); Cargo does not.

Our empirical measurements show the approach works and the speedup is large. A production implementation inside rustc/cargo (the approach prototyped by [Zed](https://github.com/zed-industries/zed)) would close the remaining soundness gaps by operating on post-expansion, type-resolved data.

## License

MIT OR Apache-2.0
