//! Chunk engine. Tokio does the I/O. Rayon decodes. The UI thread never waits.
//!
//! The UI sends the list of tiles that it needs, in priority order, at each frame. The engine
//! starts the tiles in this order and cancels the tiles that are not in the list any more.
//! Tiles go back to the UI through a channel.
use crate::Lru;
use crate::disk::Disk;
use crate::pixels::*;
use bytes::Bytes;
use eo_core::geo::Warp;
use eo_core::*;
use eo_io::{Dataset, codec};
use futures_util::future::{BoxFuture, FutureExt, WeakShared, join_all};
use futures_util::stream::{self, StreamExt};
use rayon::prelude::*;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};
use tokio::runtime::{Handle, Runtime};
use tokio::task::AbortHandle;

mod ops;
pub use ops::{Agg, Op, OpKind, StepIn};

/// Width and height of a display tile.
pub const TILE: u64 = 512;
/// Chunks in one read and decode job of a generated overview tile.
const GROUP: usize = 16;
/// Interval between the partial updates of a generated overview tile.
const PARTIAL: Duration = Duration::from_millis(60);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TileKey {
    pub layer: u64,
    pub lv: u8,
    pub tx: u32,
    pub ty: u32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LevelSrc {
    /// A level of the file (index in `Variable::levels`).
    File(usize),
    /// Made from display level `base` (a file level), with a `f` x `f` mean.
    Virtual { base: usize, f: u64 },
}

/// One level of the display pyramid. Level `d` has about 1/2^d of the level-0 size.
#[derive(Clone, Copy, Debug)]
pub struct Level {
    pub w: u64,
    pub h: u64,
    /// Level-0 pixels for each pixel of this level.
    pub kx: f64,
    pub ky: f64,
    /// Level-0 position of the top-left corner of this level.
    pub ox: f64,
    pub oy: f64,
    pub src: LevelSrc,
}

/// One displayable 2D plane of a variable: one band, or one part of a complex band.
pub struct Layer {
    /// Same id for the same dataset, variable, choice and time step. Cache keys use it.
    pub id: u64,
    pub ds: Arc<Dataset>,
    pub ds_id: u64,
    pub var: usize,
    pub choice: usize,
    pub band: u64,
    /// Step of the time dimension. 0 without a time dimension.
    pub time: u64,
    pub part: Part,
    pub levels: Vec<Level>,
    pub enc: Enc,
    /// Sorted sample of physical values (finite, not the fill value).
    pub sample: Vec<f32>,
    /// The same sample, not sorted, with NaN for no data. Two layers with the same grid have their
    /// values at the same pixels: band math can use pairs of values.
    pub sample_at: Vec<f32>,
    /// A computed layer: the operation that makes its tiles (see `ops`). None: a layer of a file.
    pub op: Option<Arc<Op>>,
}

impl Layer {
    pub fn var(&self) -> &Variable {
        &self.ds.product.vars[self.var]
    }

    pub fn size(&self) -> (u64, u64) {
        self.var().size()
    }

    pub fn is_local(&self) -> bool {
        self.ds.sources.iter().all(|s| s.is_local())
    }

    /// Names of the values that the user can select for variable `v`.
    pub fn choices(v: &Variable) -> Vec<String> {
        if v.levels[0].dtype.is_complex() {
            v.bands.iter().flat_map(|b| Part::COMPLEX.iter().map(move |(_, n)| format!("{b} {n}"))).collect()
        } else {
            v.bands.clone()
        }
    }

    fn choice(v: &Variable, c: usize) -> (u64, Part) {
        if v.levels[0].dtype.is_complex() { ((c / 4) as u64, Part::COMPLEX[c % 4].0) } else { (c as u64, Part::Real) }
    }

    /// (a, b) so that physical value = texel * a + b. For u8 data the texel is in 0..1.
    pub fn texel_to_phys(&self) -> (f32, f32) {
        let v = self.var();
        let (s, o) = if self.part == Part::Real || self.part == Part::I || self.part == Part::Q {
            (v.scale as f32, v.offset as f32)
        } else {
            (1.0, 0.0)
        };
        if self.enc.u8 { (255.0 * s, o) } else { (s / self.enc.k, self.enc.off * s + o) }
    }

    /// Fill value as a u8 texel (0..1). Other data uses NaN for no data.
    pub fn fill_texel(&self) -> Option<f32> {
        self.var().fill.filter(|_| self.enc.u8).map(|f| f as f32 / 255.0)
    }

    /// Key of generated overview tile `key` in the disk cache: the same in all sessions.
    fn disk_key(&self, key: TileKey) -> impl std::hash::Hash + Send + 'static {
        let v = self.var();
        let src = v.levels[0].chunks.iter().next().and_then(|c| self.ds.sources.get(c.src as usize)).map_or("", |s| s.name());
        (src.to_string(), v.name.clone(), self.choice, self.time, key.lv, key.tx, key.ty, self.enc.u8, self.enc.k.to_bits(), self.enc.off.to_bits())
    }

    /// Index in `Array::chunks` of chunk (cy, cx) for the band of this layer.
    fn chunk_at(&self, a: &Array, cy: u64, cx: u64) -> usize {
        chunk_at(a, self.band, self.time, cy, cx)
    }
}

/// Index in `Array::chunks` of chunk (cy, cx) that contains band `band` of time step `time`.
pub fn chunk_at(a: &Array, band: u64, time: u64, cy: u64, cx: u64) -> usize {
    let pos: Vec<u64> = a
        .dims
        .iter()
        .zip(&a.chunk)
        .map(|(d, c)| match d.as_str() {
            "y" => cy,
            "x" => cx,
            "band" => band / c,
            "time" => time / c,
            _ => 0,
        })
        .collect();
    a.chunk_index(&pos)
}

/// Display pyramid: the file levels, fine to coarse, with generated levels in the gaps (a ratio of
/// more than about 2.7 between two file levels) and below the coarsest file level (until one tile).
fn display_levels(v: &Variable) -> Vec<Level> {
    let (w0, h0) = v.size();
    let file = |i: usize| {
        let a = &v.levels[i];
        let (w, h) = (a.len_of("x"), a.len_of("y"));
        let [kx, ky, ox, oy] = a.place.unwrap_or([w0 as f64 / w as f64, h0 as f64 / h as f64, 0.0, 0.0]);
        Level { w, h, kx, ky, ox, oy, src: LevelSrc::File(i) }
    };
    let halve = |out: &Vec<Level>| {
        let base = out.iter().rposition(|l| matches!(l.src, LevelSrc::File(_))).unwrap();
        let (b, f) = (out[base], 1u64 << (out.len() - base));
        let src = LevelSrc::Virtual { base, f };
        Level { w: b.w.div_ceil(f), h: b.h.div_ceil(f), kx: b.kx * f as f64, ky: b.ky * f as f64, src, ..b }
    };
    let mut order: Vec<usize> = (0..v.levels.len()).collect();
    order.sort_by(|&a, &b| file(a).kx.total_cmp(&file(b).kx));
    let mut out: Vec<Level> = vec![];
    for i in order {
        let f = file(i);
        while out.last().is_some_and(|p| p.kx * 2.0 < f.kx * 0.75) {
            out.push(halve(&out));
        }
        if out.last().is_none_or(|p| f.kx > p.kx * 1.01) {
            out.push(f);
        }
    }
    while out.last().is_some_and(|l| l.w.max(l.h) > TILE) {
        out.push(halve(&out));
    }
    out
}

pub enum Event {
    Opened { req: u64, res: Result<Arc<Layer>> },
    /// A display tile. `done` is false for a partial tile: more data comes later.
    Tile { key: TileKey, w: u32, h: u32, px: Arc<Pixels>, done: bool },
    /// Physical values of all bands at level-0 pixel (x, y). `None` is no data.
    Probe { layer: u64, x: u64, y: u64, values: Vec<Option<f64>> },
    /// Warp grid of a layer to a display CRS (EPSG code; None: pixel space).
    Warp { layer: u64, dst: Option<u32>, res: Result<Arc<Warp>> },
    /// Values of a window of a layer (see `Engine::read`), and the georeferencing of the window.
    Read { req: u64, res: Result<(Arc<Vec<f32>>, Georef)> },
    Error(String),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub limit: usize,
    pub raw: usize,
    pub dec: usize,
    pub probe: usize,
    pub work: usize,
    pub running: usize,
    pub wanted: usize,
    /// Bytes and budget of the disk cache. Budget 0: no disk cache.
    pub disk: u64,
    pub disk_limit: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum DecKey {
    Chunk { layer: u64, lvl: usize, idx: usize },
    Tile(TileKey),
}

type Planes = std::result::Result<Arc<Vec<Arc<Pixels>>>, Error>;


#[derive(Default)]
struct Sched {
    lists: HashMap<u32, Vec<(Arc<Layer>, TileKey)>>,
    running: HashMap<TileKey, AbortHandle>,
    /// Tiles sent to the UI. The UI can ask for them again before it gets them: do not start them again.
    sent: HashSet<TileKey>,
}

struct Inner {
    /// Bytes of the raw, decoded and inspector caches (copies of the cache counters).
    used: [Arc<AtomicUsize>; 3],
    /// Running and wanted tiles (copies of the scheduler counts).
    counts: [AtomicUsize; 2],
    rt: Handle,
    pool: rayon::ThreadPool,
    jobs: Arc<Mutex<BinaryHeap<Job>>>,
    limit: usize,
    work: AtomicUsize,
    /// Encoded bytes of remote chunks.
    raw: Mutex<Lru<(u64, u32, u64), Bytes>>,
    /// Encoded bytes of remote chunks and generated overview tiles of remote layers, between sessions.
    disk: OnceLock<Arc<Disk>>,
    /// Display planes of chunks, and generated overview tiles.
    dec: Mutex<Lru<DecKey, Arc<Pixels>>>,
    /// Reads in progress. Weak: when all tiles that wait for a read are cancelled, the read stops.
    inflight: Mutex<HashMap<DecKey, (WeakShared<BoxFuture<'static, Planes>>, usize)>>,
    /// Decoded level-0 chunks for the pixel inspector.
    probe: Mutex<Lru<(u64, usize, usize), Bytes>>,
    /// Geolocation grids read from arrays, by (dataset, variable).
    grids: Mutex<HashMap<(u64, usize), Georef>>,
    sched: Mutex<Sched>,
    ids: Mutex<HashMap<(u64, usize, usize, u64), u64>>,
    next: AtomicU64,
    max_running: usize,
    /// Tiles of computed layers that run at the same time: each one reads the tiles of many inputs.
    ops: tokio::sync::Semaphore,
    tx: mpsc::Sender<Event>,
    wake: Box<dyn Fn() + Send + Sync>,
}

/// Decode job. The pool runs the job with the lowest (priority, sequence) first.
struct Job {
    key: (u32, u64),
    f: Box<dyn FnOnce() + Send>,
}

impl PartialEq for Job {
    fn eq(&self, o: &Self) -> bool {
        self.key == o.key
    }
}

impl Eq for Job {}

impl PartialOrd for Job {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Job {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        o.key.cmp(&self.key)
    }
}

/// RAM that a work buffer uses, counted while the buffer exists.
struct Charge<'a>(&'a AtomicUsize, usize);

impl<'a> Charge<'a> {
    fn new(c: &'a AtomicUsize, n: usize) -> Self {
        c.fetch_add(n, Relaxed);
        Charge(c, n)
    }
}

impl Drop for Charge<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(self.1, Relaxed);
    }
}

pub struct Engine {
    inner: Arc<Inner>,
    rt: Runtime,
}

impl Engine {
    /// `ram` is the RAM budget in bytes for all pixel data. `wake` is called after each event.
    pub fn new(ram: usize, wake: impl Fn() + Send + Sync + 'static) -> (Engine, mpsc::Receiver<Event>) {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("eo-io")
            .enable_all()
            .build()
            .expect("tokio runtime");
        // EOVIEW_THREADS: number of decode threads. Default: all physical cores but one. With SMT, a decode
        // thread on the other half of the core of the UI thread makes the frames slow.
        let n = std::env::var("EOVIEW_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(crate::physical_cores().saturating_sub(1).max(1));
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .thread_name(|i| format!("eo-decode-{i}"))
            // Lower priority: the UI thread gets the CPU first when all decode threads are busy.
            // ponytail: Linux only (there, the nice value is for each thread). Add macOS QoS and Windows thread priority if needed.
            .start_handler(|_| {
                #[cfg(target_os = "linux")]
                // SAFETY: on Linux, setpriority with PRIO_PROCESS and 0 changes only the calling thread.
                unsafe {
                    libc::setpriority(libc::PRIO_PROCESS, 0, 10);
                }
            })
            .build()
            .expect("rayon pool");
        let (tx, rx) = mpsc::channel();
        let (raw, dec, probe) = (Lru::new(ram / 4), Lru::new(ram / 2), Lru::new(ram / 16));
        let inner = Inner {
            used: [raw.used.clone(), dec.used.clone(), probe.used.clone()],
            counts: Default::default(),
            rt: rt.handle().clone(),
            max_running: 2 * threads + 16,
            ops: tokio::sync::Semaphore::new(4),
            pool,
            jobs: Default::default(),
            limit: ram,
            work: AtomicUsize::new(0),
            raw: Mutex::new(raw),
            disk: OnceLock::new(),
            dec: Mutex::new(dec),
            inflight: Default::default(),
            probe: Mutex::new(probe),
            grids: Default::default(),
            sched: Default::default(),
            ids: Default::default(),
            next: AtomicU64::new(1),
            tx,
            wake: Box::new(wake),
        };
        (Engine { inner: Arc::new(inner), rt }, rx)
    }

    /// Keep remote data in directory `dir`, with a budget of `cap` bytes. Call it one time, before `open`.
    pub fn disk_cache(&self, dir: std::path::PathBuf, cap: u64) -> std::io::Result<()> {
        let d = Arc::new(Disk::new(dir, cap)?);
        if self.inner.disk.set(d.clone()).is_ok() {
            // The count of the files that are there does not delay the start.
            self.rt.spawn_blocking(move || d.trim());
        }
        Ok(())
    }

    /// Open a local path or a URL. The result comes as `Event::Opened` with the returned request id.
    pub fn open(&self, url: String) -> u64 {
        let i = self.inner.clone();
        let req = i.next.fetch_add(1, Relaxed);
        self.rt.spawn_blocking(move || {
            let res = eo_io::open(&url, &i.rt).and_then(|ds| {
                let ds_id = i.next.fetch_add(1, Relaxed);
                i.layer(Arc::new(ds), ds_id, 0, 0, 0)
            });
            i.send(Event::Opened { req, res });
        });
        req
    }

    /// Make a layer for another variable, choice or time step of the dataset of `l`. The result comes as
    /// `Event::Opened`.
    pub fn select(&self, l: &Layer, var: usize, choice: usize, time: u64) -> u64 {
        let (i, ds, ds_id) = (self.inner.clone(), l.ds.clone(), l.ds_id);
        let req = i.next.fetch_add(1, Relaxed);
        self.rt.spawn_blocking(move || {
            let res = i.layer(ds, ds_id, var, choice, time);
            i.send(Event::Opened { req, res });
        });
        req
    }

    /// Set the tiles that `client` needs, in priority order (the most important first).
    pub fn want(&self, client: u32, tiles: Vec<(Arc<Layer>, TileKey)>) {
        self.inner.sched.lock().unwrap().lists.insert(client, tiles);
        self.inner.dispatch();
    }

    /// As `want`, for a client that keeps its own copy of the tiles (tiles on the CPU): the tiles that are
    /// new in its list come again, also if the engine sent them before for an other client.
    pub fn want_copy(&self, client: u32, tiles: Vec<(Arc<Layer>, TileKey)>) {
        let mut s = self.inner.sched.lock().unwrap();
        let old: HashSet<TileKey> = s.lists.get(&client).map_or_else(HashSet::new, |l| l.iter().map(|t| t.1).collect());
        for t in tiles.iter().filter(|t| !old.contains(&t.1)) {
            s.sent.remove(&t.1);
        }
        s.lists.insert(client, tiles);
        drop(s);
        self.inner.dispatch();
    }

    /// Read the values of all bands at level-0 pixel (x, y). The result comes as `Event::Probe`.
    pub fn probe(&self, l: Arc<Layer>, x: u64, y: u64) {
        let i = self.inner.clone();
        self.rt.spawn(async move {
            let r = match &l.op {
                Some(op) => i.probe_op(op, x, y).await,
                None => i.probe_at(&l, x, y).await,
            };
            match r {
                Ok(values) => i.send(Event::Probe { layer: l.id, x, y, values }),
                Err(e) => i.send(Event::Error(e.0)),
            }
        });
    }

    /// Make the warp grid of `l` to the display CRS `dst` (EPSG code; None: pixel space).
    /// The result comes as `Event::Warp`.
    pub fn warp(&self, l: Arc<Layer>, dst: Option<u32>) {
        let i = self.inner.clone();
        self.rt.spawn_blocking(move || {
            let (w, h) = l.size();
            let res = i.georef(&l).and_then(|g| Warp::new(&g, w as f64, h as f64, dst)).map(Arc::new);
            i.send(Event::Warp { layer: l.id, dst, res });
        });
    }

    /// Free values on the blocking pool: large buffers go back to the system with `munmap`, which can take
    /// milliseconds. The UI thread must not wait for it.
    pub fn drop_later<T: Send + 'static>(&self, v: T) {
        self.rt.spawn_blocking(move || drop(v));
    }

    /// Georeferencing of `l`, with the geolocation arrays read (blocks).
    pub fn georef(&self, l: &Layer) -> Result<Georef> {
        self.inner.georef(l)
    }

    /// Cache and scheduler counts. No lock: the UI calls it at each frame.
    pub fn stats(&self) -> Stats {
        let i = &self.inner;
        Stats {
            limit: i.limit,
            raw: i.used[0].load(Relaxed),
            dec: i.used[1].load(Relaxed),
            probe: i.used[2].load(Relaxed),
            work: i.work.load(Relaxed),
            running: i.counts[0].load(Relaxed),
            wanted: i.counts[1].load(Relaxed),
            disk: i.disk.get().map_or(0, |d| d.used.load(Relaxed)),
            disk_limit: i.disk.get().map_or(0, |d| d.cap),
        }
    }

    /// Run a future on the I/O runtime and wait for it. For tools and tests, not for the UI thread.
    pub fn block_on<T>(&self, f: impl Future<Output = T>) -> T {
        self.rt.block_on(f)
    }
}

impl Inner {
    fn send(&self, e: Event) {
        let _ = self.tx.send(e);
        (self.wake)();
    }

    /// Run `f` on the decode pool. Rayon has no priorities: each spawn runs the most important job of the
    /// queue at that time. `prio` is the rank of the tile in the lists of the UI (0 is the most important).
    /// Georeferencing of a layer. Geolocation arrays become a grid (read once, then kept).
    fn georef(&self, l: &Layer) -> Result<Georef> {
        let Georef::Arrays { lon, lat, step, off } = &l.var().georef else { return Ok(l.var().georef.clone()) };
        if let Some(g) = self.grids.lock().unwrap().get(&(l.ds_id, l.var)) {
            return Ok(g.clone());
        }
        let p = &l.ds.product;
        let find = |n: &str| p.vars.iter().position(|v| v.name == *n).ok_or_else(|| Error(format!("no geolocation variable {n}")));
        let (vlon, vlat) = (find(lon)?, find(lat)?);
        let a = &p.vars[vlon].levels[0];
        let (w, h) = (a.len_of("x"), a.len_of("y"));
        // About 256 nodes on the long side, and the last row and column.
        let s = w.max(h).div_ceil(256).max(1);
        let idx = |n: u64| -> Vec<u64> { (0..n).step_by(s as usize).chain(((n - 1) % s != 0).then_some(n - 1)).collect() };
        let (is, js) = (idx(w), idx(h));
        let lonv = self.rt.block_on(self.points(&l.ds, l.ds_id, vlon, &is, &js))?;
        let latv = self.rt.block_on(self.points(&l.ds, l.ds_id, vlat, &is, &js))?;
        let g = Georef::Grid {
            cols: is.iter().map(|&i| off[0] + i as f64 * step[0]).collect(),
            rows: js.iter().map(|&j| off[1] + j as f64 * step[1]).collect(),
            lon: lonv,
            lat: latv,
        };
        self.grids.lock().unwrap().insert((l.ds_id, l.var), g.clone());
        Ok(g)
    }

    /// Physical values of variable `var` (level 0, band 0) at columns `is` and rows `js`, in row order.
    async fn points(&self, ds: &Arc<Dataset>, ds_id: u64, var: usize, is: &[u64], js: &[u64]) -> Result<Vec<f64>> {
        let v = &ds.product.vars[var];
        let a = &v.levels[0];
        let (ch, cw) = chunk_size(a);
        let mut cells: Vec<(u64, u64)> = js.iter().flat_map(|j| is.iter().map(move |i| (j / ch, i / cw))).collect();
        cells.sort_unstable();
        cells.dedup();
        let locs: Vec<ChunkLoc> = cells.iter().map(|&(cy, cx)| a.chunks.at(chunk_at(a, 0, 0, cy, cx))).collect();
        let raws = self.raw_ds(ds, ds_id, &locs).await?;
        let (a, v2, is, js) = (a.clone(), v.clone(), is.to_vec(), js.to_vec());
        self.on_pool(0, move || {
            let dec: Vec<Result<std::borrow::Cow<[u8]>>> = raws.par_iter().map(|r| if r.is_empty() { Ok(Default::default()) } else { readable(&a, r) }).collect();
            let p = PlaneAt::new(&a, 0, 0);
            let mut out = Vec::with_capacity(is.len() * js.len());
            for &j in &js {
                for &i in &is {
                    let k = cells.binary_search(&(j / ch, i / cw)).unwrap();
                    let d = dec[k].as_ref().map_err(Clone::clone)?;
                    let e = p.base + (j % ch) as usize * p.sy + (i % cw) as usize * p.sx;
                    let x = if (e + 1) * a.dtype.size() <= d.len() { value_f64(&a, d, e, Part::Real) } else { f64::NAN };
                    out.push(if x.is_finite() && Some(x) != v2.fill { x * v2.scale + v2.offset } else { f64::NAN });
                }
            }
            Ok(out)
        })
        .await
    }

    async fn on_pool<T: Send + 'static>(&self, prio: u32, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let f = Box::new(move || {
            // A cancelled task drops the receiver: then the job does not start.
            if !tx.is_closed() {
                let _ = tx.send(f());
            }
        });
        self.jobs.lock().unwrap().push(Job { key: (prio, self.next.fetch_add(1, Relaxed)), f });
        let jobs = self.jobs.clone();
        self.pool.spawn(move || {
            let j = jobs.lock().unwrap().pop();
            if let Some(j) = j {
                (j.f)();
            }
        });
        rx.await.expect("decode job stopped")
    }

    fn layer(&self, ds: Arc<Dataset>, ds_id: u64, var: usize, choice: usize, time: u64) -> Result<Arc<Layer>> {
        let v = ds.product.vars.get(var).filter(|v| time < v.steps()).ok_or("no such variable or time step")?;
        let ((band, part), levels) = (Layer::choice(v, choice), display_levels(v));
        let id = *self.ids.lock().unwrap().entry((ds_id, var, choice, time)).or_insert_with(|| self.next.fetch_add(1, Relaxed));
        let mut l = Layer {
            id,
            levels,
            ds,
            ds_id,
            var,
            choice,
            band,
            time,
            part,
            enc: Enc { u8: false, k: 1.0, off: 0.0 },
            sample: vec![],
            sample_at: vec![],
            op: None,
        };
        l.ds.sources.iter().for_each(|s| s.sparse(true));
        let raw = self.rt.block_on(self.sample(&l));
        l.ds.sources.iter().for_each(|s| s.sparse(false));
        let raw = raw?;
        let mut sorted: Vec<f32> = raw.iter().copied().filter(|x| x.is_finite()).collect();
        sorted.sort_unstable_by(f32::total_cmp);
        l.enc = choose_enc(l.var().levels[0].dtype, part, &sorted);
        let v = l.var();
        let (s, o) = if part == Part::Amp || part == Part::Phase { (1.0, 0.0) } else { (v.scale as f32, v.offset as f32) };
        l.sample_at = raw.into_iter().map(|r| r * s + o).collect();
        l.sample = sorted.into_iter().map(|r| r * s + o).collect();
        l.sample.sort_unstable_by(f32::total_cmp);
        Ok(Arc::new(l))
    }

    /// Sample of raw values: spaced values of all chunks of the coarsest level, or of 16 chunks spread over it.
    /// NaN for no data. The positions depend only on the grid of the level.
    async fn sample(&self, l: &Layer) -> Result<Vec<f32>> {
        let v = l.var();
        let a = v.levels.last().unwrap();
        let (ch, cw) = chunk_size(a);
        let (h, w) = (a.len_of("y"), a.len_of("x"));
        let (gy, gx) = (h.div_ceil(ch), w.div_ceil(cw));
        let n = (gy * gx) as usize;
        let pick: Vec<(u64, u64)> = (0..n.min(16)).map(|i| i * n / n.min(16)).map(|i| (i as u64 / gx, i as u64 % gx)).collect();
        let locs: Vec<ChunkLoc> = pick.iter().map(|&(cy, cx)| a.chunks.at(l.chunk_at(a, cy, cx))).collect();
        let raws = self.raw_many(l, &locs).await?;
        let (a, fill, part, band, time) = (a.clone(), v.fill, l.part, l.band, l.time);
        self.on_pool(0, move || {
            let p = PlaneAt::new(&a, band, time);
            let per = 65_536 / pick.len().max(1);
            let parts: Vec<Result<Vec<f32>>> = pick
                .par_iter()
                .zip(&raws)
                .map(|(&(cy, cx), raw)| {
                    // Only the pixels inside the array: edge chunks have padding.
                    let (rows, cols) = ((h - cy * ch).min(ch) as usize, (w - cx * cw).min(cw) as usize);
                    // Up to 64 spaced rows, 16 short runs of adjacent values in each row: few memory pages
                    // of a large uncompressed chunk. The rows go to all threads: page faults wait in parallel.
                    let nr = rows.min(64);
                    let run = (per / (nr * 16)).clamp(1, cols.div_ceil(16));
                    let pos: Vec<(usize, usize)> = (0..nr)
                        .flat_map(|k| (0..16).flat_map(move |s| (s * cols / 16..(s * cols / 16 + run).min(cols)).map(move |c| (k * rows / nr, c))))
                        .collect();
                    if raw.is_empty() {
                        return Ok(vec![f32::NAN; pos.len()]);
                    }
                    let d = readable(&a, raw)?;
                    let (d, a) = (&d, &a);
                    Ok(pos
                        .par_iter()
                        .with_min_len(64)
                        .map(|&(r, c)| {
                            let i = p.base + r * p.sy + c * p.sx;
                            let x = if (i + 1) * a.dtype.size() <= d.len() { value_f64(a, d, i, part) } else { f64::NAN };
                            if x.is_finite() && Some(x) != fill { x as f32 } else { f32::NAN }
                        })
                        .collect())
                })
                .collect();
            let mut out = vec![];
            for p in parts {
                out.extend(p?);
            }
            Ok(out)
        })
        .await
    }

    /// Encoded bytes of chunks. Local data is not copied. Remote data goes through the raw byte cache.
    async fn raw_many(&self, l: &Layer, locs: &[ChunkLoc]) -> Result<Vec<Bytes>> {
        self.raw_ds(&l.ds, l.ds_id, locs).await
    }

    async fn raw_ds(&self, ds: &Dataset, ds_id: u64, locs: &[ChunkLoc]) -> Result<Vec<Bytes>> {
        // Chunks of shards: get their byte ranges from the shard indexes. A shard index is read one time,
        // at the first use of the shard.
        let resolved: Vec<ChunkLoc>;
        let locs = if locs.iter().any(|c| c.len == ChunkLoc::SHARD) {
            let r = join_all(locs.iter().map(|c| async move {
                if c.len != ChunkLoc::SHARD {
                    return Ok(*c);
                }
                let s = ds.sources.get(c.src as usize).ok_or("bad source index")?;
                Ok::<_, Error>(match s.inner_chunk(c.off).await? {
                    Some(r) => ChunkLoc { src: c.src, off: r.start, len: r.end - r.start },
                    None => ChunkLoc { src: c.src, off: 0, len: 0 },
                })
            }))
            .await;
            resolved = r.into_iter().collect::<Result<Vec<_>>>()?;
            &resolved[..]
        } else {
            locs
        };
        let mut out = vec![Bytes::new(); locs.len()];
        let (mut miss, mut whole): (HashMap<u32, Vec<usize>>, Vec<usize>) = (HashMap::new(), vec![]);
        let mut local = vec![];
        {
            let mut raw = self.raw.lock().unwrap();
            for (i, c) in locs.iter().enumerate() {
                let s = ds.sources.get(c.src as usize).ok_or("bad source index")?;
                if c.len == 0 {
                    continue;
                } else if s.is_local() {
                    local.push(i);
                } else if let Some(b) = raw.get(&(ds_id, c.src, c.off)) {
                    out[i] = b.clone();
                } else if c.len == ChunkLoc::WHOLE || c.len == ChunkLoc::KEYED {
                    whole.push(i);
                } else {
                    miss.entry(c.src).or_default().push(i);
                }
            }
        }
        // A missing object (a Zarr chunk that was not written) gives empty bytes: the fill value.
        for i in local {
            let (c, s) = (locs[i], &ds.sources[locs[i].src as usize]);
            out[i] = match c.len {
                ChunkLoc::WHOLE => s.get_whole().await?.unwrap_or_default(),
                ChunkLoc::KEYED => s.get_keyed(c.off).await?.unwrap_or_default(),
                _ => s.read(c.off..c.off + c.len)?,
            };
        }
        // Remote chunks of an earlier session come from the disk cache.
        let key = |i: usize| (ds.sources[locs[i].src as usize].name().to_string(), locs[i].off, locs[i].len);
        let mut fetched: Vec<(usize, Bytes)> = vec![];
        if let Some(d) = self.disk.get().filter(|_| !whole.is_empty() || !miss.is_empty()) {
            let want: Vec<_> = whole.iter().chain(miss.values().flatten()).map(|&i| (i, key(i))).collect();
            let d = d.clone();
            let read = move || want.into_iter().filter_map(|(i, k)| Some((i, d.get(&k)?))).collect();
            fetched = tokio::task::spawn_blocking(read).await.unwrap_or_default();
            let hit: HashSet<usize> = fetched.iter().map(|f| f.0).collect();
            whole.retain(|i| !hit.contains(i));
            miss.retain(|_, v| {
                v.retain(|i| !hit.contains(i));
                !v.is_empty()
            });
        }
        let from_disk = fetched.len();
        let got = join_all(whole.iter().map(|&i| async move {
            let (c, s) = (locs[i], &ds.sources[locs[i].src as usize]);
            if c.len == ChunkLoc::KEYED { s.get_keyed(c.off).await } else { s.get_whole().await }
        }))
        .await;
        for (&i, b) in whole.iter().zip(got) {
            fetched.push((i, b?.unwrap_or_default()));
        }
        for (src, idx) in miss {
            let ranges: Vec<_> = idx.iter().map(|&i| locs[i].off..locs[i].off + locs[i].len).collect();
            let got = ds.sources[src as usize].get_ranges(&ranges).await?;
            fetched.extend(idx.into_iter().zip(got));
        }
        if let Some(d) = self.disk.get().filter(|_| fetched.len() > from_disk) {
            let new: Vec<_> = fetched[from_disk..].iter().map(|(i, b)| (key(*i), b.clone())).collect();
            let d = d.clone();
            self.rt.spawn_blocking(move || new.iter().for_each(|(k, b)| d.put(k, b)));
        }
        let mut raw = self.raw.lock().unwrap();
        for (i, b) in fetched {
            raw.insert((ds_id, locs[i].src, locs[i].off), b.clone(), b.len());
            out[i] = b;
        }
        Ok(out)
    }

    /// Display planes of chunks `idxs` of file level `lvl`. Two tiles that need the same chunk share one read.
    async fn planes(self: &Arc<Self>, l: &Arc<Layer>, lvl: usize, idxs: &[usize], prio: u32) -> Result<Vec<Arc<Pixels>>> {
        let mut out = vec![None; idxs.len()];
        let mut waits = vec![];
        {
            let (mut dec, mut inf) = (self.dec.lock().unwrap(), self.inflight.lock().unwrap());
            let mut claim = vec![];
            for (i, &idx) in idxs.iter().enumerate() {
                let k = DecKey::Chunk { layer: l.id, lvl, idx };
                if let Some(p) = dec.get(&k) {
                    out[i] = Some(p.clone());
                } else if let Some((b, j)) = inf.get(&k).and_then(|(b, j)| Some((b.upgrade()?, *j))) {
                    waits.push((i, b, j));
                } else {
                    claim.push((i, idx));
                }
            }
            if !claim.is_empty() {
                let ci: Vec<usize> = claim.iter().map(|c| c.1).collect();
                let b = self.clone().batch(l.clone(), lvl, ci, prio).boxed().shared();
                for (j, &(i, idx)) in claim.iter().enumerate() {
                    inf.insert(DecKey::Chunk { layer: l.id, lvl, idx }, (b.downgrade().unwrap(), j));
                    waits.push((i, b.clone(), j));
                }
            }
        }
        let done = join_all(waits.iter().map(|w| w.1.clone())).await;
        for ((i, _, j), r) in waits.iter().zip(done) {
            out[*i] = Some(r?[*j].clone());
        }
        Ok(out.into_iter().map(Option::unwrap).collect())
    }

    async fn batch(self: Arc<Self>, l: Arc<Layer>, lvl: usize, idxs: Vec<usize>, prio: u32) -> Planes {
        let a = &l.var().levels[lvl];
        let locs: Vec<ChunkLoc> = idxs.iter().map(|&i| a.chunks.at(i)).collect();
        let res = match self.raw_many(&l, &locs).await {
            Ok(raws) => {
                let l2 = l.clone();
                let _c = Charge::new(&self.work, a.chunk_bytes() * raws.len());
                self.on_pool(prio, move || {
                    let a = &l2.var().levels[lvl];
                    let p = PlaneAt::new(a, l2.band, l2.time);
                    let fill = l2.var().fill;
                    raws.par_iter()
                        .map(|raw| {
                            if raw.is_empty() {
                                return Ok(Arc::new(Pixels::empty(&l2.enc, fill, p.ch * p.cw)));
                            }
                            let d = codec::decode(a, raw)?;
                            Ok(Arc::new(encode(a, &d, &p, l2.part, &l2.enc, fill, &p.full())))
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .await
            }
            Err(e) => Err(e),
        };
        let (mut dec, mut inf) = (self.dec.lock().unwrap(), self.inflight.lock().unwrap());
        for (j, &idx) in idxs.iter().enumerate() {
            let k = DecKey::Chunk { layer: l.id, lvl, idx };
            inf.remove(&k);
            if let Ok(v) = &res {
                dec.insert(k, v[j].clone(), v[j].size());
            }
        }
        res.map(Arc::new)
    }

    fn dispatch(self: &Arc<Self>) {
        let mut s = self.sched.lock().unwrap();
        let s = &mut *s;
        // Merge the lists of all clients, rank by rank.
        let lists: Vec<&Vec<(Arc<Layer>, TileKey)>> = s.lists.values().collect();
        let n = lists.iter().map(|l| l.len()).max().unwrap_or(0);
        let merged: Vec<&(Arc<Layer>, TileKey)> = (0..n).flat_map(|r| lists.iter().filter_map(move |l| l.get(r))).collect();
        let wanted: HashSet<TileKey> = merged.iter().map(|t| t.1).collect();
        s.running.retain(|k, h| {
            let keep = wanted.contains(k);
            if !keep {
                h.abort();
            }
            keep
        });
        s.sent.retain(|k| wanted.contains(k));
        for (rank, (l, key)) in merged.into_iter().enumerate() {
            if s.running.len() >= self.max_running {
                break;
            }
            if s.running.contains_key(key) || s.sent.contains(key) {
                continue;
            }
            let (i, l, key) = (self.clone(), l.clone(), *key);
            let h = self.rt.spawn(async move {
                if let Err(e) = i.tile(&l, key, rank as u32).await {
                    i.send(Event::Error(format!("{}: {e}", l.var().name)));
                }
                {
                    let mut s = i.sched.lock().unwrap();
                    s.running.remove(&key);
                    s.sent.insert(key);
                }
                i.dispatch();
            });
            s.running.insert(key, h.abort_handle());
        }
        self.counts[0].store(s.running.len(), Relaxed);
        self.counts[1].store(s.lists.values().map(|l| l.len()).sum(), Relaxed);
    }

    async fn tile(self: &Arc<Self>, l: &Arc<Layer>, key: TileKey, prio: u32) -> Result<()> {
        let lv = l.levels.get(key.lv as usize).ok_or("bad level")?;
        let (x0, y0) = (key.tx as u64 * TILE, key.ty as u64 * TILE);
        if x0 >= lv.w || y0 >= lv.h {
            return Err("tile outside the image".into());
        }
        let (w, h) = (TILE.min(lv.w - x0), TILE.min(lv.h - y0));
        let send = |px, done| self.send(Event::Tile { key, w: w as u32, h: h as u32, px, done });
        if let Some(op) = &l.op {
            let px = self.op_tile(l, op, key, prio, &send).await?;
            send(px, true);
            return Ok(());
        }
        match lv.src {
            LevelSrc::File(lvl) => {
                let a = &l.var().levels[lvl];
                let (ch, cw) = chunk_size(a);
                let cells: Vec<(u64, u64)> =
                    (y0 / ch..=(y0 + h - 1) / ch).flat_map(|cy| (x0 / cw..=(x0 + w - 1) / cw).map(move |cx| (cy, cx))).collect();
                let idxs: Vec<usize> = cells.iter().map(|&(cy, cx)| l.chunk_at(a, cy, cx)).collect();
                if a.codecs.is_empty() {
                    // Uncompressed: read the tile region from the chunk bytes. The memory map is the cache.
                    let locs: Vec<ChunkLoc> = idxs.iter().map(|&i| a.chunks.at(i)).collect();
                    let raws = self.raw_many(l, &locs).await?;
                    let l2 = l.clone();
                    let t = self
                        .on_pool(prio, move || {
                            let a = &l2.var().levels[lvl];
                            let p = PlaneAt::new(a, l2.band, l2.time);
                            let mut t = Pixels::empty(&l2.enc, l2.var().fill, (w * h) as usize);
                            for (&(cy, cx), raw) in cells.iter().zip(&raws) {
                                let (ry0, ry1) = ((cy * ch).max(y0), ((cy + 1) * ch).min(y0 + h));
                                let (rx0, rx1) = ((cx * cw).max(x0), ((cx + 1) * cw).min(x0 + w));
                                let win = Win {
                                    r0: (ry0 - cy * ch) as usize,
                                    r1: (ry1 - cy * ch) as usize,
                                    c0: (rx0 - cx * cw) as usize,
                                    c1: (rx1 - cx * cw) as usize,
                                };
                                if raw.is_empty() {
                                    continue;
                                }
                                if !p.covers(a, raw.len(), &win) {
                                    return Err(Error("chunk data is shorter than the chunk".into()));
                                }
                                let px = encode(a, raw, &p, l2.part, &l2.enc, l2.var().fill, &win);
                                t.copy_from(((ry0 - y0) * w + rx0 - x0) as usize, w as usize, &px, 0, win.w(), win.w(), win.h());
                            }
                            Ok(t)
                        })
                        .await?;
                    send(Arc::new(t), true);
                    return Ok(());
                }
                let planes = self.planes(l, lvl, &idxs, prio).await?;
                let mut t = Pixels::empty(&l.enc, l.var().fill, (w * h) as usize);
                for (&(cy, cx), p) in cells.iter().zip(&planes) {
                    let (ry0, ry1) = ((cy * ch).max(y0), ((cy + 1) * ch).min(y0 + h));
                    let (rx0, rx1) = ((cx * cw).max(x0), ((cx + 1) * cw).min(x0 + w));
                    let d0 = ((ry0 - y0) * w + rx0 - x0) as usize;
                    let s0 = ((ry0 - cy * ch) * cw + rx0 - cx * cw) as usize;
                    t.copy_from(d0, w as usize, p, s0, cw as usize, (rx1 - rx0) as usize, (ry1 - ry0) as usize);
                }
                send(Arc::new(t), true);
            }
            LevelSrc::Virtual { base, f } => {
                let tk = DecKey::Tile(key);
                if let Some(p) = self.dec.lock().unwrap().get(&tk).cloned() {
                    send(p, true);
                    return Ok(());
                }
                // A generated tile of a remote layer stays on the disk: the next open does not read its chunks.
                let disk = self.disk.get().filter(|_| !l.is_local()).cloned();
                let mut px = None;
                if let Some(d) = disk.clone() {
                    let (k, u8) = (l.disk_key(key), l.enc.u8);
                    let read = move || d.get(&k).and_then(|b| Pixels::from_bytes(u8, &b, (w * h) as usize));
                    px = tokio::task::spawn_blocking(read).await.ok().flatten().map(Arc::new);
                }
                let px = match px {
                    Some(px) => px,
                    None => {
                        let (sum, cnt) = self.generate(l, base, f, (x0, y0, w, h), prio, &send).await?;
                        let px = Arc::new(mean(l, &sum, &cnt));
                        if let Some(d) = disk {
                            let (k, p) = (l.disk_key(key), px.clone());
                            self.rt.spawn_blocking(move || d.put(&k, p.bytes()));
                        }
                        px
                    }
                };
                self.dec.lock().unwrap().insert(tk, px.clone(), px.size());
                send(px, true);
            }
        }
        Ok(())
    }

    /// Make an overview tile: the `f` x `f` mean of display level `base`, as the sums and the numbers of the
    /// valid values of each pixel. Send partial tiles while the chunks arrive.
    /// `t` is the tile rectangle (x, y, width, height) at the overview level.
    async fn generate(
        self: &Arc<Self>,
        l: &Arc<Layer>,
        base: usize,
        f: u64,
        t: (u64, u64, u64, u64),
        prio: u32,
        send: &impl Fn(Arc<Pixels>, bool),
    ) -> Result<(Vec<f64>, Vec<u32>)> {
        let (x0, y0, w, h) = t;
        let LevelSrc::File(lvl) = l.levels[base].src else { unreachable!("base of an overview is a file level") };
        let a = &l.var().levels[lvl];
        let (bw, bh) = (l.levels[base].w, l.levels[base].h);
        let (ch, cw) = chunk_size(a);
        let (bx0, by0, bx1, by1) = (x0 * f, y0 * f, ((x0 + w) * f).min(bw), ((y0 + h) * f).min(bh));
        // Work units: rows of one chunk. A compressed chunk is one unit (it is decoded completely).
        // An uncompressed chunk is cut into units of about 1 M pixels: a large strip is not read at once.
        let cols = (bx1 - bx0).min(cw);
        let step = if a.codecs.is_empty() { ((1 << 20) / cols).max(1) } else { ch };
        let mut units: Vec<Unit> = vec![];
        for cy in by0 / ch..=(by1 - 1) / ch {
            let (r0, r1) = ((cy * ch).max(by0), ((cy + 1) * ch).min(by1));
            for cx in bx0 / cw..=(bx1 - 1) / cw {
                units.extend((r0..r1).step_by(step as usize).map(|r| Unit { cy, cx, r0: r, r1: (r + step).min(r1) }));
            }
        }
        // Center first: the partial tiles fill from the center.
        let (mx, my) = ((bx0 + bx1) as f64 / 2.0, (by0 + by1) as f64 / 2.0);
        let dist = |u: &Unit| ((u.cx as f64 + 0.5) * cw as f64 - mx).powi(2) + ((u.r0 + u.r1) as f64 / 2.0 - my).powi(2);
        units.sort_by(|p, q| dist(p).total_cmp(&dist(q)));

        let n = (w * h) as usize;
        let _c = Charge::new(&self.work, n * 12);
        let (mut sum, mut cnt) = (vec![0f64; n], vec![0u32; n]);
        let region = Region { bx0, by0, bx1, f, w: w as usize };
        // Groups of one unit for each decode thread. Few groups at a time: the first partial tile comes
        // after one group, not after all groups. Remote data has more groups in flight for the latency.
        let threads = self.pool.current_num_threads();
        let per = threads.min(GROUP);
        let inflight = if l.is_local() { 2 } else { 8 };
        // Owned groups and clones: borrowed data in the stream closures does not satisfy the spawn bounds.
        // The first group is small: the first partial tile comes sooner.
        let first = units.len().min(2);
        let groups: Vec<Vec<Unit>> = std::iter::once(units[..first].to_vec()).chain(units[first..].chunks(per).map(|g| g.to_vec())).collect();
        let mut jobs = stream::iter(groups)
            .map(|g| {
                let (i, l) = (self.clone(), l.clone());
                async move {
                    let a = &l.var().levels[lvl];
                    let locs: Vec<ChunkLoc> = g.iter().map(|u| a.chunks.at(l.chunk_at(a, u.cy, u.cx))).collect();
                    let raws = i.raw_many(&l, &locs).await?;
                    let _c = Charge::new(&i.work, if a.codecs.is_empty() { 0 } else { a.chunk_bytes() * g.len() });
                    let l2 = l.clone();
                    i.on_pool(prio, move || {
                        let a = &l2.var().levels[lvl];
                        g.par_iter().zip(&raws).map(|(u, raw)| partial(&l2, a, raw, u, &region)).collect::<Result<Vec<_>>>()
                    })
                    .await
                }
            })
            .buffer_unordered(inflight);
        // The first partial tile goes out after the first job.
        let mut last = Instant::now() - PARTIAL;
        while let Some(r) = jobs.next().await {
            for p in r?.into_iter().flatten() {
                for j in 0..p.ph {
                    let d = (p.j0 + j) * region.w + p.i0;
                    for i in 0..p.pw {
                        sum[d + i] += p.sum[j * p.pw + i];
                        cnt[d + i] += p.cnt[j * p.pw + i];
                    }
                }
            }
            if last.elapsed() > PARTIAL {
                send(Arc::new(mean(l, &sum, &cnt)), false);
                last = Instant::now();
            }
        }
        Ok((sum, cnt))
    }

    async fn probe_at(&self, l: &Layer, x: u64, y: u64) -> Result<Vec<Option<f64>>> {
        let v = l.var();
        let a = &v.levels[0];
        let (w, h) = v.size();
        if x >= w || y >= h {
            return Ok(vec![]);
        }
        let (ch, cw) = chunk_size(a);
        let mut out = vec![];
        for band in 0..a.len_of("band") {
            let idx = chunk_at(a, band, l.time, y / ch, x / cw);
            let key = (l.ds_id, l.var, idx);
            let cached = self.probe.lock().unwrap().get(&key).cloned();
            let d = match cached {
                Some(d) => d,
                None => {
                    let raw = self.raw_many(l, &[a.chunks.at(idx)]).await?.remove(0);
                    if raw.is_empty() {
                        out.push(None);
                        continue;
                    }
                    if a.codecs.is_empty() {
                        raw
                    } else {
                        let a2 = a.clone();
                        let d = Bytes::from(self.on_pool(0, move || codec::decode(&a2, &raw).map(|d| d.into_owned())).await?);
                        self.probe.lock().unwrap().insert(key, d.clone(), d.len());
                        d
                    }
                }
            };
            let p = PlaneAt::new(a, band, l.time);
            let i = p.base + (y % ch) as usize * p.sy + (x % cw) as usize * p.sx;
            if (i + 1) * a.dtype.size() > d.len() {
                return Err("chunk data is shorter than the chunk".into());
            }
            let raw = value_f64(a, &d, i, l.part);
            let phys = if matches!(l.part, Part::Amp | Part::Phase) { raw } else { raw * v.scale + v.offset };
            out.push((raw.is_finite() && Some(raw) != v.fill).then_some(phys));
        }
        Ok(out)
    }
}

/// Footprint of an overview tile at its base level.
#[derive(Clone, Copy)]
struct Region {
    bx0: u64,
    by0: u64,
    bx1: u64,
    f: u64,
    w: usize,
}

/// Rows `r0..r1` (level pixels) of chunk (cy, cx).
#[derive(Clone, Copy)]
struct Unit {
    cy: u64,
    cx: u64,
    r0: u64,
    r1: u64,
}

/// Sums and counts of valid values of one work unit, for the overview tile pixels that the unit touches.
struct Partial {
    i0: usize,
    j0: usize,
    pw: usize,
    ph: usize,
    sum: Vec<f64>,
    cnt: Vec<u32>,
}

fn partial(l: &Layer, a: &Array, raw: &[u8], u: &Unit, r: &Region) -> Result<Option<Partial>> {
    let p = PlaneAt::new(a, l.band, l.time);
    let (ch, cw) = (p.ch as u64, p.cw as u64);
    let (oy, ox) = (u.cy * ch, u.cx * cw);
    let (ry0, ry1, rx0, rx1) = (u.r0, u.r1, ox.max(r.bx0), (ox + cw).min(r.bx1));
    if raw.is_empty() || ry0 >= ry1 || rx0 >= rx1 {
        return Ok(None);
    }
    let d = readable(a, raw)?;
    let win = Win { r0: (ry0 - oy) as usize, r1: (ry1 - oy) as usize, c0: (rx0 - ox) as usize, c1: (rx1 - ox) as usize };
    if !p.covers(a, d.len(), &win) {
        return Err("chunk data is shorter than the chunk".into());
    }
    let s = r.f.trailing_zeros();
    let (j0, i0) = (((ry0 - r.by0) >> s) as usize, ((rx0 - r.bx0) >> s) as usize);
    let (j1, i1) = ((((ry1 - 1 - r.by0) >> s) + 1) as usize, (((rx1 - 1 - r.bx0) >> s) + 1) as usize);
    let (pw, ph) = (i1 - i0, j1 - j0);
    let (mut sum, mut cnt) = (vec![0f64; pw * ph], vec![0u32; pw * ph]);
    let fill = l.var().fill.map(|f| f as f32);
    let mut row = vec![0f32; win.w()];
    for y in ry0..ry1 {
        let wr = Win { r0: (y - oy) as usize, r1: (y - oy + 1) as usize, ..win };
        to_f32(a, &d, &p, l.part, &wr, &mut row);
        let j = ((y - r.by0) >> s) as usize - j0;
        for (x, &val) in (rx0..rx1).zip(&row) {
            if val.is_finite() && Some(val) != fill {
                let i = j * pw + ((x - r.bx0) >> s) as usize - i0;
                sum[i] += val as f64;
                cnt[i] += 1;
            }
        }
    }
    Ok(Some(Partial { i0, j0, pw, ph, sum, cnt }))
}

/// Values of a chunk that the region functions can read: the chunk bytes for uncompressed data
/// (not copied), or the decoded bytes.
fn readable<'a>(a: &Array, raw: &'a [u8]) -> Result<std::borrow::Cow<'a, [u8]>> {
    if a.codecs.is_empty() { Ok(std::borrow::Cow::Borrowed(raw)) } else { codec::decode(a, raw) }
}

/// Chunk height and width.
fn chunk_size(a: &Array) -> (u64, u64) {
    (a.chunk[a.axis("y").unwrap()], a.chunk[a.axis("x").unwrap()])
}

fn mean(l: &Layer, sum: &[f64], cnt: &[u32]) -> Pixels {
    let fill = l.var().fill;
    if l.enc.u8 {
        let nd = fill.map_or(0, |f| f as u8);
        return Pixels::U8(sum.iter().zip(cnt).map(|(&s, &c)| if c == 0 { nd } else { (s / c as f64).round() as u8 }).collect());
    }
    let mut f: Vec<f32> = sum.iter().zip(cnt).map(|(&s, &c)| if c == 0 { f32::NAN } else { (s / c as f64) as f32 }).collect();
    Pixels::F16(encode_f32(&mut f, &l.enc, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(w: u64, h: u64, levels: &[(u64, u64)]) -> Variable {
        let arr = |w: u64, h: u64| Array {
            dims: vec!["y".into(), "x".into()],
            shape: vec![h, w],
            chunk: vec![256, 256],
            dtype: DType::U16,
            le: true,
            codecs: vec![],
            chunks: vec![].into(),
            place: None,
        };
        Variable {
            times: Default::default(),
            name: "t".into(),
            group: String::new(),
            levels: std::iter::once(arr(w, h)).chain(levels.iter().map(|&(w, h)| arr(w, h))).collect(),
            bands: vec!["b".into()],
            fill: None,
            scale: 1.0,
            offset: 0.0,
            units: String::new(),
            georef: Georef::None,
        }
    }

    #[test]
    fn levels_with_other_ratios() {
        // EOPF multiscales: 10, 20, 60, 120 m. A level at 40 m fills the gap between 20 and 60 m.
        let mut v = var(10980, 10980, &[(5490, 5490), (1830, 1830), (915, 915)]);
        for (a, k) in v.levels.iter_mut().zip([1.0, 2.0, 6.0, 12.0]) {
            a.place = Some([k, k, 0.0, 0.0]);
        }
        let l = display_levels(&v);
        let k: Vec<f64> = l.iter().map(|l| l.kx).collect();
        assert_eq!(k, [1.0, 2.0, 4.0, 6.0, 12.0, 24.0]);
        assert_eq!(l[2].src, LevelSrc::Virtual { base: 1, f: 2 });
    }

    #[test]
    fn levels_use_file_overviews_and_fill_gaps() {
        // COG-like: overviews at 2 and 8. Level 2 (factor 4) is generated from level 1.
        let l = display_levels(&var(4001, 3000, &[(2001, 1500), (501, 375)]));
        let src: Vec<LevelSrc> = l.iter().map(|l| l.src).collect();
        assert_eq!(src, [LevelSrc::File(0), LevelSrc::File(1), LevelSrc::Virtual { base: 1, f: 2 }, LevelSrc::File(2)]);
        assert_eq!((l[2].w, l[2].h), (1001, 750));
        assert!((l[2].kx - 4001.0 / 2001.0 * 2.0).abs() < 1e-9);
        let l = display_levels(&var(300, 200, &[]));
        assert_eq!(l.len(), 1);
    }
}
