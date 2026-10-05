//! Byte sources. A local file is memory-mapped: reads are zero-copy slices.
//! A remote object (HTTP, HTTPS, S3) uses range requests through `object_store`.
//!
//! S3 (`s3://bucket/key`): the configuration comes from the AWS environment variables (AWS_ACCESS_KEY_ID,
//! AWS_SECRET_ACCESS_KEY, AWS_SESSION_TOKEN, AWS_REGION, AWS_ENDPOINT_URL) and from the profile AWS_PROFILE
//! (or `default`) of the AWS configuration files. Without credentials the requests have no signature
//! (public buckets). The credentials stay in the store: they are not in logs, caches or error messages.
use bytes::Bytes;
use eo_core::{Error, Result};
use eo_core::ChunkLoc;
use object_store::{GetOptions, GetRange, ObjectStore, ObjectStoreExt, path::Path};
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
    /// The source is a shard of the Zarr sharding codec: number of inner chunks, true if the index is at
    /// the end, bytes of the index checksum.
    shard: Option<(u64, bool, u64)>,
    /// Shard index: (offset, length) of each inner chunk. Empty: the shard does not exist.
    index: tokio::sync::OnceCell<Vec<u64>>,
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

/// Store of an HTTP origin (`https://host`) or of an S3 bucket (`s3://bucket`).
fn store(origin: &str, rt: &Handle) -> Result<Arc<dyn ObjectStore>> {
    let mut m = STORES.lock().unwrap();
    if let Some(s) = m.get(origin) {
        return Ok(s.clone());
    }
    let err = |e: object_store::Error| Error(format!("{origin}: {e}"));
    let opts = object_store::ClientOptions::new().with_allow_http(true);
    let s: Arc<dyn ObjectStore> = match origin.strip_prefix("s3://") {
        Some(bucket) => Arc::new(object_store::limit::LimitStore::new(s3(bucket, opts, rt).map_err(err)?, PER_HOST)),
        None => {
            let http = object_store::http::HttpBuilder::new().with_url(origin).with_client_options(opts).build().map_err(err)?;
            Arc::new(object_store::limit::LimitStore::new(http, PER_HOST))
        }
    };
    m.insert(origin.into(), s.clone());
    Ok(s)
}

/// Keys of a profile in the AWS configuration files (`~/.aws/credentials` and `~/.aws/config`, or the files
/// that AWS_SHARED_CREDENTIALS_FILE and AWS_CONFIG_FILE give).
fn aws_profile() -> HashMap<String, String> {
    let profile = std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".into());
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(std::path::PathBuf::from).unwrap_or_default();
    let file = |var: &str, name: &str| std::env::var_os(var).map_or_else(|| home.join(".aws").join(name), Into::into);
    let mut out = HashMap::new();
    for f in [file("AWS_CONFIG_FILE", "config"), file("AWS_SHARED_CREDENTIALS_FILE", "credentials")] {
        ini_section(&std::fs::read_to_string(f).unwrap_or_default(), &profile, &mut out);
    }
    out
}

/// Keys of section `[profile]` or `[profile <profile>]` of an AWS configuration file.
fn ini_section(text: &str, profile: &str, out: &mut HashMap<String, String>) {
    let mut on = false;
    for l in text.lines().map(str::trim) {
        if let Some(sec) = l.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            on = sec.trim().trim_start_matches("profile ").trim() == profile;
        } else if let Some((k, v)) = l.split_once('=').filter(|_| on && !l.starts_with(['#', ';'])) {
            out.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
}

fn s3(bucket: &str, opts: object_store::ClientOptions, rt: &Handle) -> object_store::Result<object_store::aws::AmazonS3> {
    use object_store::aws::{AmazonS3Builder, AmazonS3ConfigKey as K};
    let mut cfg: HashMap<K, String> = HashMap::new();
    // The environment variables are stronger than the profile.
    let env = std::env::vars().filter(|(k, _)| k.starts_with("AWS_")).map(|(k, v)| (k.to_lowercase(), v));
    for (k, v) in aws_profile().into_iter().chain(env) {
        if let Ok(k) = k.parse::<K>() {
            cfg.insert(k, v);
        }
    }
    let has = |k: K| cfg.contains_key(&k);
    let signed = has(K::AccessKeyId) || has(K::ContainerCredentialsRelativeUri) || has(K::ContainerCredentialsFullUri) || has(K::WebIdentityTokenFile);
    let aws = !has(K::Endpoint) && !has(K::S3Endpoint);
    let mut b = AmazonS3Builder::new().with_bucket_name(bucket).with_client_options(opts.clone());
    if !has(K::Region) && !has(K::DefaultRegion) {
        // An AWS bucket answers only in its region: ask for it (one request for each bucket). On a thread
        // without a runtime: the caller can be a task. Other S3 services do not use the region.
        let region = if aws { std::thread::scope(|s| s.spawn(|| rt.block_on(object_store::aws::resolve_bucket_region(bucket, &opts))).join().unwrap())? } else { "default".into() };
        b = b.with_region(region);
    }
    for (k, v) in cfg {
        b = b.with_config(k, v);
    }
    // ponytail: no credentials from the EC2 instance metadata service: without keys, the bucket is public.
    b.with_skip_signature(!signed).build()
}

pub fn is_remote(url: &str) -> bool {
    ["http://", "https://", "s3://"].iter().any(|s| url.starts_with(s))
}

impl Source {
    /// Source for a local path or an HTTP(S) URL. No I/O: the source opens at the first use.
    pub fn new(url: &str, rt: &Handle) -> Source {
        Source { name: url.into(), rt: rt.clone(), state: OnceLock::new(), len: OnceLock::new(), shard: None, index: Default::default() }
    }

    /// Source for a shard of the Zarr sharding codec with `n` inner chunks. No I/O: the index is read at
    /// the first use of a chunk.
    pub fn shard(url: &str, rt: &Handle, n: u64, index_at_end: bool, checksum: u64) -> Source {
        Source { shard: Some((n, index_at_end, checksum)), ..Source::new(url, rt) }
    }

    /// Byte range of inner chunk `k` of a shard. None: the shard or the chunk does not exist (fill value).
    /// The first call reads the index: one request, a suffix range for an index at the end.
    pub async fn inner_chunk(&self, k: u64) -> Result<Option<Range<u64>>> {
        let (n, end, crc) = self.shard.ok_or_else(|| Error(format!("{}: not a shard", self.name)))?;
        let idx = self
            .index
            .get_or_try_init(|| async {
                let size = n * 16 + crc;
                let b = match self.inner()? {
                    Inner::Missing => None,
                    Inner::File { map, .. } if (map.len() as u64) < size => None,
                    Inner::File { map, .. } => {
                        let at = if end { map.len() - size as usize } else { 0 };
                        Some(map.slice(at..at + (n * 16) as usize))
                    }
                    Inner::Remote { store, path } => {
                        let range = if end { GetRange::Suffix(size) } else { GetRange::Bounded(0..size) };
                        match store.get_opts(path, GetOptions::new().with_range(Some(range))).await {
                            Ok(r) => Some(r.bytes().await.map_err(|e| Error(format!("{}: {e}", self.name)))?),
                            Err(object_store::Error::NotFound { .. }) => None,
                            Err(e) => return Err(Error(format!("{}: shard index: {e}", self.name))),
                        }
                    }
                };
                // u64 little-endian pairs. u64::MAX: no chunk.
                let v: Vec<u64> = b.iter().flat_map(|b| b.chunks_exact(8)).take(2 * n as usize).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect();
                Ok::<_, Error>(if v.len() == 2 * n as usize { v } else { vec![] })
            })
            .await?;
        Ok(match (idx.get(2 * k as usize), idx.get(2 * k as usize + 1)) {
            (Some(&o), Some(&l)) if o != u64::MAX => Some(o..o + l),
            _ => None,
        })
    }

    /// Encoded bytes of the chunk at `loc` in this source. Empty: the chunk does not exist. Blocks for a
    /// remote source: for tools, tests and the open of a product, not for the engine.
    pub fn read_chunk(&self, loc: ChunkLoc) -> Result<Bytes> {
        match loc.len {
            0 => Ok(Bytes::new()),
            ChunkLoc::WHOLE => Ok(self.rt.block_on(self.get_whole())?.unwrap_or_default()),
            ChunkLoc::SHARD => match self.rt.block_on(self.inner_chunk(loc.off))? {
                Some(r) => self.read(r),
                None => Ok(Bytes::new()),
            },
            len => self.read(loc.off..loc.off + len),
        }
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
                let origin = if u.scheme() == "s3" { format!("s3://{}", u.host_str().unwrap_or("")) } else { u.origin().ascii_serialization() };
                Ok(Inner::Remote { store: store(&origin, &self.rt)?, path })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_of_aws_files() {
        let text = "[default]\nregion = eu-west-1\n# note\n[profile cdse]\nendpoint_url = https://eodata.example\naws_access_key_id=K\n[other]\nregion = x\n";
        let mut m = HashMap::new();
        ini_section(text, "cdse", &mut m);
        assert_eq!(m.len(), 2);
        assert_eq!((m["endpoint_url"].as_str(), m["aws_access_key_id"].as_str()), ("https://eodata.example", "K"));
        let mut m = HashMap::new();
        ini_section(text, "default", &mut m);
        assert_eq!(m, HashMap::from([("region".to_string(), "eu-west-1".to_string())]));
    }
}
