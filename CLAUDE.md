# CLAUDE.md / AGENTS.md

This file provides guidance to code agents when working with code in this
repository.

## Tactical Docs

**IMPORTANT** When you need information about development of this application,
check the `devdocs/` directory, especially the `_INDEX.md` for curated tactical
knowledge we've already learned. If we learn new things as part of implementing
a feature, we need to keep these docs updated.

## Critical Rules

**IMPORTANT: Be extremely concise.** Sacrifice grammar for concision. List
unresolved questions at end.

**Use built-in tools for file operations.** Use Glob for file search, Grep for
content search, Read for viewing files. Do not request grep/sed/fd/find/ls/cat
or similar CLI tools—you already have these capabilities built-in.

**Read code before modifying it.** Understand existing patterns and context
before proposing changes.

**Prefer minimal diffs.** Only change what is necessary. Avoid refactoring or
"improving" code beyond what was requested.

**Git commits must be attributed to the developer, not the agent. No agent
co-authors.**

## When Working with Rust Code

### Development Commands

```bash
# Check compilation and clippy lints (code incomplete if this fails, including warnings)
cargo lint

# Format code (code incomplete if not formatted)
cargo fmt
```

Run both `cargo lint` and `cargo fmt` at WORKSPACE level before calling a
feature complete.

### CRITICAL: API Visibility Rules

**Always use the narrowest visibility possible:**

```rust
// AVOID: Public by default
pub struct MyStruct;
pub fn my_function() {}

// PREFER: Narrowest visibility
pub(crate) struct MyStruct;  // Only if needed by other modules in crate
fn my_function() {}          // Private by default

// OK: Public methods on crate-visible structs (still crate-scoped)
pub(crate) struct CrateScopedStruct;
impl CrateScopedStruct {
    pub fn hello_world() {}
}
```

**Priority order:** (1) no modifier (private), (2) `pub(super)`, (3)
`pub(crate)`, (4) `pub` (only for actual public API)

**Rationale:** Enables dead code detection at compile time, makes API surface
explicit, prevents accidental exposure, improves compilation speed.

### Data Modeling Rules

- Prefer restructuring code over using `Arc<Mutex<...>>`. Consult developer if
  cross-thread sync is needed.

### Style Rules

- Prefer `#[expect(lint, reason = "...")]` over `#[allow(lint)]`. If you must
  use `allow`, add `// [TODO] @<developer>: fix allow lint`.
- Use item-level imports, not nested crate/module imports.
- Prefer `use` statements at module top over inline imports.
- Only comment unintuitive or "weird" code. Avoid obvious comments like "create
  a loop" above a for-loop.
- Avoid mutable variables. Prefer new bindings or shadowing over mutation.
- Never re-export. Use individual use statements where needed instead of
  `pub use` or `pub(crate) use`.
- Never employ a wildcard `use` statement on an enum when trying to shorten
  match arms. Doing this is a subtle catch-all-match-arm footgun. e.g.

  ```rust
  enum SomeLongName {
      A,
      B
  }

  // AVOID: wildcard "use" statement
  use SomeLongName::*;
  match value {
      A => 1,
      B => 2
  }

  // PREFER: short "use" alias
  use SomeLongName as S;
  match value {
      S::A => 1,
      S::B => 2
  }
  ```

### Testing Rules

Do not write tests unless explicitly asked. Assume developer will test manually.

### Safety Rules

Never use `unsafe` without asking.

### Dependency Rules

- When adding dependencies, use `cargo add` to ensure we install the latest
  version of dependencies rather than outdated versions from your training data.

# Extra Rust Development Commands

```bash
# Format code and also fix nested imports
cargo +nightly fmt -- --config imports_granularity='Item' && cargo fmt
```

# Linting Rule

**IMPORTANT** Always run `cargo lint` and `cargo fmt` from the workspace root,
and do not limit to a single crate. We always want linting and formatting to run
on the whole workspace, not just a single crate.

# Style Rule

Put logging variables inside the interpolated string and not as arguments to the
macro whenever possible. Example:

```rust
// PREFER
tracing::error!("Status {MAX_RETRIES} attempts: {e}, {obj:?}");

// DO NOT PREFER
tracing::error!("Status {} attempts: {}, {:?}", MAX_RETRIES, e, obj);
```

# Extra General Instructions

If you need to create intermediate artifacts for yourself, like a one-off shell
script, analysis document, etc, place it in a `.deleteme` directory in the
current working directory. This will automatically be gitignored and you will
not need to prompt for permissions to read/write to it.
