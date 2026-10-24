# LogKit

## TL;DR

One logging format for every app, behind a tiny zero-dependency DLL: structured one-line records (`2026-10-23T19:50:43.123Z INFO  net: connected host=example.com attempt=2`), size-based rolling files with a hard size budget, and crash-safety by construction — every record is flushed as it's written, and rotation never splits a record across files. Open a logger in one call, from the first line of `main`, and the log is there when you go hunting — not bolted on after the hunt comes up empty.

```c
LkLogger *log;
lk_open("app.log", LK_INFO, 5 * 1024 * 1024, 3, 0, &log);   // 5 MB, keep 3 backups
LK_INFO(log, "app", "started");
lk_log_fields(log, LK_WARN, "net", "connected",
              "{\"host\":\"example.com\",\"attempt\":2}");
lk_close(log);
```

## Why this exists

Every app in the family had reinvented logging in a different format — different timestamps, different level names, different file habits — so nothing transferred: no shared tooling, no shared instincts when reading a user's log at 1 a.m. And in the worst case there was no log at all until one got bolted on after a launch failure had already burned the debugging session (WinDevKit pain point #14; the CrucibleKit milestone log was exactly that bolt-on). LogKit makes the log a constant:

- **One format, forever.** UTC ISO-8601 with milliseconds, a fixed-width level column (TRACE/DEBUG/INFO/WARN/ERROR), a target naming the subsystem, the message, then structured `key=value` fields. Readable in Notepad; parseable with a split. The format is the product — it does not grow options.
- **Structured without a database.** Fields come in as a flat JSON object and land as `key=value` pairs — bare when safe (`attempt=2`), quoted with escapes when not (`msg="a b"`). Nested values are rejected loudly: a log line is not a document store.
- **Crash-safe by construction.** Flush-per-record means a process crash loses at most the record being written. Rotation is checked *before* a write that would overflow, so no record ever straddles two files, and the current file never exceeds its budget by more than the single record that crossed it. For logs that must also survive an *OS* crash, `LK_SYNC_ON_WRITE` adds a disk sync per record — off by default, because flush already covers app crashes and sync costs real speed.
- **A hard disk budget.** `max_bytes` + `keep_files` caps total log footprint (`app.log`, `app.log.1`, …). A log file shares a disk with the user's data; it behaves accordingly.
- **Thread-safe handle.** Real apps log from worker threads. Writes are serialized inside the library, so any thread can log through one handle without the app building its own lock.

## The API in one screen

| Area | Calls |
|---|---|
| Lifecycle | `lk_open` (path, level floor, size budget, backups, flags), `lk_close` |
| Writing | `lk_log`, `lk_log_fields` (flat-JSON fields), plus `LK_INFO(...)`-style macros for the five levels |
| Control | `lk_set_level` (change the floor at runtime), `lk_enabled` (cheap check before building an expensive message), `lk_flush` |
| Errors | Integer `LK_*` codes + thread-local `lk_last_error()`; a filtered record is `LK_OK` — filtering is not an error |

Levels run TRACE=1 … ERROR=5. Records below the floor are dropped silently. Timestamps are UTC — one timezone in every log from every machine, so two logs from two PCs line up without arithmetic.

## Honest caveats

- **One writer per file.** Two processes appending to the same log file will interleave at record granularity at best — point each process at its own file (single-instance apps, the norm, are unaffected).
- **Flush ≠ sync.** The default guarantees records survive your *process* dying. Surviving the *OS* or power dying needs `LK_SYNC_ON_WRITE`, and you will feel its cost on chatty paths.
- **Rotation is rename-based.** If another program holds a file in the chain open with a deny-write handle (some editors do), rotation reports `LK_ERR_IO` — but the record is still written to the reopened log file, and the logger never wedges; the file may exceed its budget until a later rotation succeeds.
- **The format is fixed.** No per-app timestamp styles, level words, or delimiters — that rigidity is the feature being bought.

## Verification status

12 tests cover the civil-time conversion (epoch, leap day, pre-epoch, year boundary — cross-checked against system `date`), the record shape and fixed-width level column, one-line message escaping, the flat-fields parser (including nested-value and duplicate-key rejection), value rendering (bare vs quoted), rolling behaviour (rotation before overflow, backup budget, oversized single record written whole, keep=0 discard), and a full FFI end-to-end through real files (level filtering, fields, parse error writes nothing). Windows CI runs the full WinDevKit gate — format, clippy, the whole suite, and a real MSVC build and link.

Building: `cargo build --release` → `logkit.dll` / `liblogkit.so`; `cargo test --locked`.

License: MIT. Part of the WinDevKit family — see `AGENTS.md` for build discipline and `ROADMAP.md` for what's next.
