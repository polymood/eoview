//! Byte sources. A local file is memory-mapped: reads are zero-copy slices.
//! A remote object (HTTP, HTTPS) uses range requests through `object_store`.
use bytes::Bytes;
use eo_core::{Error, Result};
use object_store::{ObjectStore, ObjectStoreExt, path::Path};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use tokio::runtime::Handle;

/// Maximum number of parallel requests to one host.
const PER_HOST: usize = 16;

/// A local file or a remote object. It opens at the first use: a product can have many sources
/// (for example one for each Zarr chunk).
pub struct Source {
    name: String,
    rt: Handle,
    state: OnceLock<std::result::Result<Inner, Error>>,
    len: OnceLock<std::result::Result<u64, Error>>,
}

enum Inner {
    File { map: Bytes, mmap: Option<Arc<memmap2::Mmap>> },
    /// A local file that does not exist. For a chunk, this means: all values are the fill value.
    Missing,
    Remote { store: Arc<dyn ObjectStore>, path: Path },
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
    /// Source for a local path or an HTTP(S) URL. No I/O: the source opens at the first use.
    pub fn new(url: &str, rt: &Handle) -> Source {
        Source { name: url.into(), rt: rt.clone(), state: OnceLock::new(), len: OnceLock::new() }
    }

    /// Open a source now, and get its length. For a remote source this blocks: do not call it on the UI thread.
    pub fn open(url: &str, rt: &Handle) -> Result<Source> {
        let s = Source::new(url, rt);
        if let Inner::Missing = s.inner()? {
            return Err(Error(format!("{url}: file not found")));
        }
        s.len()?;
        Ok(s)
    }

    fn inner(&self) -> Result<&Inner> {
        let url = &self.name;
        self.state
            .get_or_init(|| {
                if !is_remote(url) {
                    let f = match std::fs::File::open(url) {
                        Ok(f) => f,
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Inner::Missing),
                        Err(e) => return Err(Error(format!("{url}: {e}"))),
                    };
                    if f.metadata()?.len() == 0 {
                        return Ok(Inner::File { map: Bytes::new(), mmap: None });
                    }
                    // SAFETY: read-only map. If another process truncates the file, access fails with SIGBUS.
                    let mmap = Arc::new(unsafe { memmap2::Mmap::map(&f) }?);
                    return Ok(Inner::File { map: Bytes::from_owner(Map(mmap.clone())), mmap: Some(mmap) });
                }
                let u = url::Url::parse(url).map_err(|e| Error(format!("{url}: {e}")))?;
                let path = Path::from_url_path(u.path()).map_err(|e| Error(format!("{url}: {e}")))?;
                Ok(Inner::Remote { store: store(&u.origin().ascii_serialization())?, path })
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Length in bytes. For a remote source the first call blocks (HEAD request).
    pub fn len(&self) -> Result<u64> {
        self.len
            .get_or_init(|| match self.inner()? {
                Inner::File { map, .. } => Ok(map.len() as u64),
                Inner::Missing => Ok(0),
                Inner::Remote { store, path } => {
                    let (s, p) = (store.clone(), path.clone());
                    let m = self.rt.block_on(async move { s.head(&p).await });
                    m.map(|m| m.size).map_err(|e| Error(format!("{}: {e}", self.name)))
                }
            })
            .clone()
    }

    pub fn is_local(&self) -> bool {
        !is_remote(&self.name)
    }

    /// Set the read-ahead of the page faults of a local file. `random` true: a fault reads only its page,
    /// for sparse reads (header, sample). False: the kernel reads ahead, for reads of large regions.
    pub fn sparse(&self, random: bool) {
        #[cfg(unix)]
        if let Ok(Inner::File { mmap: Some(m), .. }) = self.inner() {
            let _ = m.advise(if random { memmap2::Advice::Random } else { memmap2::Advice::Normal });
        }
        #[cfg(not(unix))]
        let _ = random;
    }

    /// Read one range. For a remote source this blocks: do not call it on the UI thread or in the runtime.
    pub fn read(&self, r: Range<u64>) -> Result<Bytes> {
        match self.inner()? {
            Inner::File { map, .. } => slice(&self.name, map, &r),
            Inner::Missing => Err(Error(format!("{}: file not found", self.name))),
            Inner::Remote { .. } => self.rt.block_on(self.get_ranges(std::slice::from_ref(&r))).map(|mut v| v.remove(0)),
        }
    }

    /// Read ranges. Adjacent ranges of a remote source merge into fewer requests.
    pub async fn get_ranges(&self, rs: &[Range<u64>]) -> Result<Vec<Bytes>> {
        match self.inner()? {
            Inner::File { map, .. } => rs.iter().map(|r| slice(&self.name, map, r)).collect(),
            Inner::Missing => Err(Error(format!("{}: file not found", self.name))),
            Inner::Remote { store, path } => store.get_ranges(path, rs).await.map_err(|e| Error(format!("{}: {e}", self.name))),
        }
    }

    /// All bytes of the source. None if the file or the object does not exist.
    pub async fn get_whole(&self) -> Result<Option<Bytes>> {
        match self.inner()? {
            Inner::File { map, .. } => Ok(Some(map.clone())),
            Inner::Missing => Ok(None),
            Inner::Remote { store, path } => match store.get(path).await {
                Ok(r) => r.bytes().await.map(Some).map_err(|e| Error(format!("{}: {e}", self.name))),
                Err(object_store::Error::NotFound { .. }) => Ok(None),
                Err(e) => Err(Error(format!("{}: {e}", self.name))),
            },
        }
    }

    /// All bytes of the source, or an error if it does not exist. Blocks for a remote source.
    pub fn read_whole(&self) -> Result<Bytes> {
        match self.rt.block_on(self.get_whole())? {
            Some(b) => Ok(b),
            None => Err(Error(format!("{}: not found", self.name))),
        }
    }
}

fn slice(name: &str, map: &Bytes, r: &Range<u64>) -> Result<Bytes> {
    if r.start > r.end || r.end > map.len() as u64 {
        return Err(Error(format!("{name}: range {r:?} is after end of data ({} bytes)", map.len())));
    }
    Ok(map.slice(r.start as usize..r.end as usize))
}
