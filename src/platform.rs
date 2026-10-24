//! Platform layer: the rolling file sink.
//!
//! Deliberately std-only — appending to a file and renaming files
//! works the same everywhere, and the sink stays testable on any
//! OS. The one Windows wrinkle handled here: `fs::rename` will not
//! replace an existing file on Windows, so rotation removes the
//! destination first (rotation targets are ours alone).
//!
//! Crash-safety contract: every record is written whole (one
//! write of line + '\n') and flushed immediately. Rotation is
//! checked BEFORE a write, so a record never straddles two files
//! and the current file never exceeds max_bytes by more than the
//! one record that crossed the line (a single record larger than
//! max_bytes is written whole to a fresh file — truncating a
//! record would be worse than exceeding the budget once).

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{LkResult, ERR_IO};

pub struct FileSink {
    path: PathBuf,
    file: Option<File>,
    current_size: u64,
    max_bytes: u64,
    keep: u32,
    sync: bool,
}

impl FileSink {
    /// Open (creating parent directories and the file as needed)
    /// and append. `max_bytes == 0` disables rotation. `keep` is
    /// the number of backup files to retain (`path.1` … `path.keep`);
    /// `keep == 0` means rotation discards the old file.
    pub fn open(path: &Path, max_bytes: u64, keep: u32, sync: bool) -> LkResult<FileSink> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .map_err(|e| (ERR_IO, format!("creating {}: {e}", parent.display())))?;
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| (ERR_IO, format!("opening {}: {e}", path.display())))?;
        let current_size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(FileSink {
            path: path.to_path_buf(),
            file: Some(file),
            current_size,
            max_bytes,
            keep,
            sync,
        })
    }

    /// Write one complete record line (no trailing newline in
    /// `line`; it is added here) and flush immediately.
    ///
    /// If rotation fails (e.g. an editor holds a backup file open
    /// with a deny-write handle), the sink reopens the log file,
    /// the record is STILL written, and the rotation error is
    /// returned so the caller knows housekeeping failed — a
    /// logging problem must never silently eat the record that
    /// reported it. The file may exceed its budget until a later
    /// rotation succeeds.
    pub fn write_record(&mut self, line: &str) -> LkResult<()> {
        let bytes = (line.len() + 1) as u64;
        let mut rotation_error: Option<(i32, String)> = None;
        if self.max_bytes > 0 && self.current_size > 0 && self.current_size + bytes > self.max_bytes
        {
            if let Err(e) = self.rotate() {
                rotation_error = Some(e);
            }
        }
        let file = self
            .file
            .as_mut()
            .ok_or((ERR_IO, "log file is closed".to_string()))?;
        file.write_all(line.as_bytes())
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.flush())
            .map_err(|e| (ERR_IO, format!("writing {}: {e}", self.path.display())))?;
        if self.sync {
            file.sync_data()
                .map_err(|e| (ERR_IO, format!("syncing {}: {e}", self.path.display())))?;
        }
        self.current_size += bytes;
        match rotation_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    pub fn flush(&mut self) -> LkResult<()> {
        match self.file.as_mut() {
            Some(f) => f
                .flush()
                .map_err(|e| (ERR_IO, format!("flushing {}: {e}", self.path.display()))),
            None => Ok(()),
        }
    }

    /// Shift the backup chain and start a fresh current file.
    /// Best-effort with a hard guarantee: whatever happens to the
    /// chain, the sink ends this call with the log path open for
    /// appending (the original file if the chain could not move),
    /// so a failed rotation never wedges the logger. Returns the
    /// first chain error encountered, if any.
    fn rotate(&mut self) -> LkResult<()> {
        // Close the current file before renaming it.
        drop(self.file.take());
        let mut first_error: Option<(i32, String)> = None;
        let mut note = |r: LkResult<()>| {
            if first_error.is_none() {
                if let Err(e) = r {
                    first_error = Some(e);
                }
            }
        };
        if self.keep == 0 {
            let _ = fs::remove_file(&self.path);
        } else {
            let oldest = suffixed(&self.path, self.keep);
            if oldest.exists() {
                note(
                    fs::remove_file(&oldest)
                        .map_err(|e| (ERR_IO, format!("removing {}: {e}", oldest.display()))),
                );
            }
            for i in (1..self.keep).rev() {
                let from = suffixed(&self.path, i);
                if from.exists() {
                    let to = suffixed(&self.path, i + 1);
                    note(rename_over(&from, &to));
                }
            }
            if self.path.exists() {
                note(rename_over(&self.path, &suffixed(&self.path, 1)));
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| (ERR_IO, format!("reopening {}: {e}", self.path.display())))?;
        self.current_size = file.metadata().map(|m| m.len()).unwrap_or(0);
        self.file = Some(file);
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// `app.log` + 2 -> `app.log.2`
fn suffixed(path: &Path, n: u32) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

fn rename_over(from: &Path, to: &Path) -> LkResult<()> {
    if to.exists() {
        fs::remove_file(to).map_err(|e| (ERR_IO, format!("removing {}: {e}", to.display())))?;
    }
    fs::rename(from, to).map_err(|e| {
        (
            ERR_IO,
            format!("renaming {} -> {}: {e}", from.display(), to.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "logkit-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn writes_and_appends() {
        let dir = temp_dir("append");
        let path = dir.join("app.log");
        let mut sink = FileSink::open(&path, 0, 3, false).unwrap();
        sink.write_record("one").unwrap();
        sink.write_record("two").unwrap();
        drop(sink);
        // Reopening appends; it does not truncate.
        let mut sink = FileSink::open(&path, 0, 3, false).unwrap();
        sink.write_record("three").unwrap();
        drop(sink);
        assert_eq!(read(&path), "one\ntwo\nthree\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rotates_before_overflow_and_keeps_budget() {
        let dir = temp_dir("rotate");
        let path = dir.join("app.log");
        // Each record is 4 bytes + newline = 5. Budget 12 fits two.
        let mut sink = FileSink::open(&path, 12, 2, false).unwrap();
        for i in 0..7 {
            sink.write_record(&format!("rec{i}")).unwrap();
        }
        drop(sink);
        let cur = read(&path);
        let b1 = read(&dir.join("app.log.1"));
        let b2 = read(&dir.join("app.log.2"));
        assert!(!dir.join("app.log.3").exists(), "keep=2 must cap backups");
        // No record is split: every file's lines are whole records.
        for content in [&cur, &b1, &b2] {
            for line in content.lines() {
                assert!(
                    line.starts_with("rec") && line.len() == 4,
                    "split record: {line:?}"
                );
            }
        }
        // keep=2 means the oldest window (rec0, rec1) was discarded
        // at the third rotation; the newest five survive, in order.
        let all = format!("{b2}{b1}{cur}");
        assert_eq!(all, "rec2\nrec3\nrec4\nrec5\nrec6\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn oversized_record_written_whole() {
        let dir = temp_dir("oversize");
        let path = dir.join("app.log");
        let mut sink = FileSink::open(&path, 8, 1, false).unwrap();
        sink.write_record("a record much longer than the budget")
            .unwrap();
        sink.write_record("next").unwrap();
        drop(sink);
        // The oversized record sits whole in the backup; the new
        // record started a fresh current file.
        assert_eq!(
            read(&dir.join("app.log.1")),
            "a record much longer than the budget\n"
        );
        assert_eq!(read(&path), "next\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keep_zero_discards() {
        let dir = temp_dir("keep0");
        let path = dir.join("app.log");
        let mut sink = FileSink::open(&path, 5, 0, false).unwrap();
        sink.write_record("aaaa").unwrap(); // exactly 5 with newline
        sink.write_record("bbbb").unwrap(); // forces rotation, old discarded
        drop(sink);
        assert_eq!(read(&path), "bbbb\n");
        assert!(!dir.join("app.log.1").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
