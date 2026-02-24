# dycon

> [!WARNING]
> This entire project is AI slop and untested as of now.

FUSE passthrough filesystem that dynamically injects content into files without modifying them on disk.

Mounts over a directory and transparently proxies all filesystem operations. For files matching specified glob patterns, reads are intercepted: the real file content is returned with extra content appended from a SQLite database.

## Usage

```bash
dycon --dir /path/to/project --db /tmp/dycon.db --intercept 'CLAUDE.md' --intercept '*.config'
```

| Flag          | Description                                              |
| ------------- | -------------------------------------------------------- |
| `--dir`       | Directory to mount over                                  |
| `--db`        | Path to SQLite database (must be outside the mount tree) |
| `--intercept` | Glob pattern for files to intercept (repeatable)         |

Ctrl-C to unmount.

## How it works

1. Opens the target directory fd **before** mounting (avoids deadlock)
2. Mounts a FUSE filesystem over `--dir` with `AutoUnmount`
3. All filesystem ops (read, write, create, rename, etc.) pass through to the real directory via `*at()` syscalls relative to the root fd
4. When reading an intercepted file, assembles: `<real file content>\n<extra content from DB>`
5. `getattr` reports the inflated size so readers see the correct file length

## SQLite schema

```sql
CREATE TABLE snippets (
    id INTEGER PRIMARY KEY,
    filename TEXT NOT NULL,     -- relative path within the mount (e.g. "CLAUDE.md")
    content TEXT NOT NULL,
    sort_order INTEGER DEFAULT 0
);
```

Multiple snippets per filename are concatenated (ordered by `sort_order`), then appended to the real file content.

## Building

```bash
cargo build --release
```

Requires Linux with FUSE support (`libfuse3-dev` or equivalent).
