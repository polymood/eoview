//! Disk cache for remote data: encoded chunk bytes and generated overview tiles. One file for each entry.
//! The file name is a hash of the key: the cache contains no URL (a URL can contain credentials).
//! When the cache is over its budget, the oldest files go first. A read makes a file new again.
// ponytail: no check that the remote object is the same as before (no ETag). Products do not change.
// Delete the cache directory to read all data again.
use bytes::Bytes;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::SystemTime;

pub struct Disk {
    dir: PathBuf,
    pub cap: u64,
    pub used: AtomicU64,
    busy: AtomicBool,
    seq: AtomicU64,
}

impl Disk {
    /// Cache in directory `dir` with a budget of `cap` bytes. Call `trim` to count the files that are there.
    pub fn new(dir: PathBuf, cap: u64) -> std::io::Result<Disk> {
        std::fs::create_dir_all(&dir)?;
        Ok(Disk { dir, cap, used: AtomicU64::new(0), busy: AtomicBool::new(false), seq: AtomicU64::new(0) })
    }

    fn path(&self, key: &impl Hash) -> PathBuf {
        // ponytail: the hash can change with the Rust version. Then the old entries are not found, and they go out by age.
        let h = |salt: u8| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (salt, key).hash(&mut h);
            h.finish()
        };
        self.dir.join(format!("{:016x}{:016x}", h(0), h(1)))
    }

    pub fn get(&self, key: &impl Hash) -> Option<Bytes> {
        let p = self.path(key);
        let b = std::fs::read(&p).ok()?;
        let _ = std::fs::File::options().write(true).open(&p).and_then(|f| f.set_modified(SystemTime::now()));
        Some(b.into())
    }

    pub fn put(&self, key: &impl Hash, data: &[u8]) {
        let p = self.path(key);
        // Write, then rename: a reader never gets a part of an entry.
        let tmp = p.with_extension(format!("{}-{}", std::process::id(), self.seq.fetch_add(1, Relaxed)));
        if std::fs::write(&tmp, data).and_then(|_| std::fs::rename(&tmp, &p)).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        if self.used.fetch_add(data.len() as u64, Relaxed) + data.len() as u64 > self.cap {
            self.trim();
        }
    }

    /// Count the bytes of the files. Over the budget: remove the oldest files, down to 90 % of the budget.
    pub fn trim(&self) {
        if self.busy.swap(true, Relaxed) {
            return;
        }
        let mut files: Vec<(SystemTime, u64, PathBuf)> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                m.is_file().then(|| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len(), e.path()))
            })
            .collect();
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        if total > self.cap {
            files.sort();
            for (_, len, p) in files {
                if total <= self.cap / 10 * 9 {
                    break;
                }
                if std::fs::remove_file(&p).is_ok() {
                    total -= len;
                }
            }
        }
        self.used.store(total, Relaxed);
        self.busy.store(false, Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_entries_and_removes_the_oldest() {
        let dir = std::env::temp_dir().join(format!("eoview-disk-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let d = Disk::new(dir.clone(), 250).unwrap();
        assert!(d.get(&("a", 1u64)).is_none());
        d.put(&("a", 1u64), &[1; 100]);
        d.put(&("a", 2u64), &[2; 100]);
        // An entry without bytes is a chunk that does not exist: it is an entry too.
        d.put(&("none", 0u64), &[]);
        assert_eq!(d.get(&("a", 1u64)).unwrap()[..], [1; 100]);
        assert_eq!(d.get(&("none", 0u64)).unwrap().len(), 0);
        // Entry 2 is the oldest (entry 1 was read): the third entry removes it.
        let old = SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options().write(true).open(d.path(&("a", 2u64))).unwrap().set_modified(old).unwrap();
        d.put(&("a", 3u64), &[3; 100]);
        assert!(d.get(&("a", 2u64)).is_none() && d.get(&("a", 1u64)).is_some() && d.get(&("a", 3u64)).is_some());
        assert_eq!(d.used.load(Relaxed), 200);
        // A new cache on the same directory counts the files.
        let d = Disk::new(dir.clone(), 250).unwrap();
        d.trim();
        assert_eq!(d.used.load(Relaxed), 200);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
