//! Byte sources. A local file is memory-mapped: reads are zero-copy slices.
//! A remote object (HTTP, HTTPS) uses range requests through `object_store`.
use bytes::Bytes;
use eo_core::{Error, Result};
use object_store::{ObjectStore, ObjectStoreExt, path::Path};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex};
use tokio::runtime::Handle;

/// Maximum number of parallel requests to one host.
const PER_HOST: usize = 16;

pub enum Source {
    File { name: String, map: Bytes, mmap: Arc<memmap2::Mmap> },
    Remote { name: String, store: Arc<dyn ObjectStore>, path: Path, len: u64, rt: Handle },
}

/// Owner of the memory map for `Bytes`.
struct Map(Arc<memmap2::Mmap>);

impl AsRef<[u8]> for Map {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// One store for each host. All requests to a host share the same request limit.
static STORES: LazyLock<Mutex<HashMap<String, Arc<dyn ObjectStore>>>> = LazyLock::new(Default::default);

fn store(origin: &str) -> Result<Arc<dyn ObjectStore>> {
    let mut m = STORES.lock().unwrap();
    if let Some(s) = m.get(origin) {
        return Ok(s.clone());
    }
    let opts = object_store::ClientOptions::new().with_allow_http(true);
    let http = object_store::http::HttpBuilder::new()
        .with_url(origin)
        .with_client_options(opts)
        .build()
        .map_err(|e| Error(format!("{origin}: {e}")))?;
    let s: Arc<dyn ObjectStore> = Arc::new(object_store::limit::LimitStore::new(http, PER_HOST));
    m.insert(origin.into(), s.clone());
    Ok(s)
}

pub fn is_remote(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

impl Source {
    /// Open a local path or an HTTP(S) URL. `rt` is necessary for remote sources.
    /// For a remote source this function blocks: do not call it on the UI thread.
    pub fn open(url: &str, rt: &Handle) -> Result<Source> {
        if !is_remote(url) {
            let f = std::fs::File::open(url).map_err(|e| Error(format!("{url}: {e}")))?;
            if f.metadata()?.len() == 0 {
                return Err(Error(format!("{url}: empty file")));
            }
            // SAFETY: read-only map. If another process truncates the file, access fails with SIGBUS.
            let mmap = Arc::new(unsafe { memmap2::Mmap::map(&f) }?);
            return Ok(Source::File { name: url.into(), map: Bytes::from_owner(Map(mmap.clone())), mmap });
        }
        let u = url::Url::parse(url).map_err(|e| Error(format!("{url}: {e}")))?;
        let origin = u.origin().ascii_serialization();
        let path = Path::from_url_path(u.path()).map_err(|e| Error(format!("{url}: {e}")))?;
        let store = store(&origin)?;
        let (s, p) = (store.clone(), path.clone());
        let len = rt.block_on(async move { s.head(&p).await }).map_err(|e| Error(format!("{url}: {e}")))?.size;
        Ok(Source::Remote { name: url.into(), store, path, len, rt: rt.clone() })
    }

    pub fn name(&self) -> &str {
        match self {
            Source::File { name, .. } | Source::Remote { name, .. } => name,
        }
    }

    pub fn len(&self) -> u64 {
        match self {
            Source::File { map, .. } => map.len() as u64,
            Source::Remote { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Set the read-ahead of the page faults of a local file. `random` true: a fault reads only its page,
    /// for sparse reads (header, sample). False: the kernel reads ahead, for reads of large regions.
    pub fn sparse(&self, random: bool) {
        #[cfg(unix)]
        if let Source::File { mmap, .. } = self {
            let _ = mmap.advise(if random { memmap2::Advice::Random } else { memmap2::Advice::Normal });
        }
        #[cfg(not(unix))]
        let _ = random;
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Source::File { .. })
    }

    fn check(&self, r: &Range<u64>) -> Result<()> {
        if r.start > r.end || r.end > self.len() {
            return Err(Error(format!("{}: range {r:?} is after end of data ({} bytes)", self.name(), self.len())));
        }
        Ok(())
    }

    /// Read one range. For a remote source this blocks: do not call it on the UI thread or in the runtime.
    pub fn read(&self, r: Range<u64>) -> Result<Bytes> {
        self.check(&r)?;
        match self {
            Source::File { map, .. } => Ok(map.slice(r.start as usize..r.end as usize)),
            Source::Remote { rt, .. } => rt.block_on(self.get_ranges(std::slice::from_ref(&r))).map(|mut v| v.remove(0)),
        }
    }

    /// Read ranges. Adjacent ranges of a remote source merge into fewer requests.
    pub async fn get_ranges(&self, rs: &[Range<u64>]) -> Result<Vec<Bytes>> {
        for r in rs {
            self.check(r)?;
        }
        match self {
            Source::File { map, .. } => Ok(rs.iter().map(|r| map.slice(r.start as usize..r.end as usize)).collect()),
            Source::Remote { store, path, name, .. } => {
                store.get_ranges(path, rs).await.map_err(|e| Error(format!("{name}: {e}")))
            }
        }
    }
}
