# LogKit Roadmap

## v0.1.0 (current)
The house log format behind a C ABI: fixed-shape structured lines (UTC ISO-8601 ms, fixed-width levels, target, key=value fields from flat JSON), size-based rolling with a hard disk budget, flush-per-record crash safety with opt-in per-record disk sync, thread-safe handle, runtime level floor.

## Next, in order
1. **A reader/parser companion** (`lk_parse` helpers + a small `loggrep` example): the format is parseable by design; shipping the parser makes that a promise instead of a hope, and gives GoldenKit a canonical consumer.
2. **Multi-sink fan-out**: one logger writing to file + a second destination (debugger output on Windows via OutputDebugString) for development builds.
3. **Per-target level floors** (`net=DEBUG, *=INFO` style spec string): grows by demand from real apps, not speculation.

## Explicitly not planned
- Format customization (timestamp styles, level words, delimiters): the rigidity is the product.
- Network/syslog shipping: a log file any tool can read beats a protocol every tool must speak; shippers can tail the file.
- Async write queues: flush-per-record simplicity is the crash-safety argument; an app that out-writes a file has a different problem.
