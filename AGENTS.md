# LogKit — agent notes

- The line format is contractual and frozen: UTC ISO-8601 millis, fixed-width level, `target: message`, then key=value fields. Any change to the shape is a breaking format change — treat it like an ABI break, not a tweak.
- Rotation happens BEFORE the overflowing write (platform.rs write_record), never after — that's what guarantees a record never straddles two files. Do not "optimize" it into a post-write check.
- Flush-per-record is the crash-safety story; LK_SYNC_ON_WRITE (sync_data per record) is opt-in for OS-crash survival. Don't enable it by default — it costs real throughput.
- Fields are FLAT JSON only; nested values and duplicate keys are parse errors on purpose. The renderer quotes a string only when it must (space/quote/backslash/=/control) — keep bare values bare.
- Gate: WinDevKit tools/gate.sh (fmt + clippy both targets + full tests). Check/clippy do not link — CI's real build is the link gate.
