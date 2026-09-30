//! Chunk engine. Tokio does the I/O. Rayon decodes. The UI thread never waits.
//!
//! The UI sends the list of tiles that it needs, in priority order, at each frame. The engine
//! starts the tiles in this order and cancels the tiles that are not in the list any more.
//! Tiles go back to the UI through a channel.
use crate::Lru;
use crate::pixels::*;
use bytes::Bytes;
use eo_core::*;
use eo_io::{Dataset, codec};
use futures_util::future::{BoxFuture, FutureExt, Shared, join_all};
use futures_util::stream::{self, StreamExt};
use rayon::prelude::*;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use tokio::runtime::{Handle, Runtime};
use tokio::task::AbortHandle;

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
    pub src: LevelSrc,
}

/// One displayable 2D plane of a variable: one band, or one part of a complex band.
pub struct Layer {
    /// Same id for the same dataset, variable and choice. Cache keys use it.
    pub id: u64,
    pub ds: Arc<Dataset>,
    pub ds_id: u64,
    pub var: usize,
    pub choice: usize,
    pub band: u64,
    pub part: Part,
    pub levels: Vec<Level>,
    pub enc: Enc,
    /// Sorted sample of physical values (finite, not the fill value).
    pub sample: Vec<f32>,
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

    /// Index in `Array::chunks` of chunk (cy, cx) for the band of this layer.
    fn chunk_at(&self, a: &Array, cy: u64, cx: u64) -> usize {
        chunk_at(a, self.band, cy, cx)
    }
}

/// Index in `Array::chunks` of chunk (cy, cx) that contains band `band`.
pub fn chunk_at(a: &Array, band: u64, cy: u64, cx: u64) -> usize {
    let pos: Vec<u64> = a
        .dims
        .iter()
        .zip(&a.chunk)
        .map(|(d, c)| match d.as_str() {
            "y" => cy,
            "x" => cx,
            "band" => band / c,
            _ => 0,
        })
        .collect();
    a.chunk_index(&pos)
}

fn display_levels(v: &Variable) -> Vec<Level> {
    let (w0, h0) = v.size();
    let (mut out, mut w, mut h) = (Vec::<Level>::new(), w0, h0);
    loop {
        let file = v.levels.iter().position(|a| a.len_of("x").abs_diff(w) <= 1 && a.len_of("y").abs_diff(h) <= 1);
        let l = match file {
            Some(i) => {
                let (lw, lh) = (v.levels[i].len_of("x"), v.levels[i].len_of("y"));
                Level { w: lw, h: lh, kx: w0 as f64 / lw as f64, ky: h0 as f64 / lh as f64, src: LevelSrc::File(i) }
            }
            None => {
                let base = out.iter().rposition(|l| matches!(l.src, LevelSrc::File(_))).unwrap();
                let (b, f) = (out[base], 1u64 << (out.len() - base));
                let src = LevelSrc::Virtual { base, f };
                Level { w: b.w.div_ceil(f), h: b.h.div_ceil(f), kx: b.kx * f as f64, ky: b.ky * f as f64, src }
            }
        };
        out.push(l);
        if w.max(h) <= TILE {
            return out;
        }
        (w, h) = (w.div_ceil(2), h.div_ceil(2));
    }
}

pub enum Event {
    Opened { req: u64, res: Result<Arc<Layer>> },
    /// A display tile. `done` is false for a partial tile: more data comes later.
    Tile { key: TileKey, w: u32, h: u32, px: Arc<Pixels>, done: bool },
    /// Physical values of all bands at level-0 pixel (x, y). `None` is no data.
    Probe { layer: u64, x: u64, y: u64, values: Vec<Option<f64>> },
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
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum DecKey {
    Chunk { layer: u64, lvl: usize, idx: usize },
    Tile(TileKey),
}

type Planes = std::result::Result<Arc<Vec<Arc<Pixels>>>, Error>;
type Batch = Shared<BoxFuture<'static, Planes>>;

#[derive(Default)]
struct Sched {
    lists: HashMap<u32, Vec<(Arc<Layer>, TileKey)>>,
    running: HashMap<TileKey, AbortHandle>,
    /// Tiles sent to the UI. The UI can ask for them again before it gets them: do not start them again.
    sent: HashSet<TileKey>,
}

struct Inner {
    rt: Handle,
    pool: rayon::ThreadPool,
    jobs: Arc<Mutex<BinaryHeap<Job>>>,
    limit: usize,
    work: AtomicUsize,
    /// Encoded bytes of remote chunks.
    raw: Mutex<Lru<(u64, u32, u64), Bytes>>,
    /// Display planes of chunks, and generated overview tiles.
    dec: Mutex<Lru<DecKey, Arc<Pixels>>>,
    inflight: Mutex<HashMap<DecKey, (Batch, usize)>>,
    /// Decoded level-0 chunks for the pixel inspector.
    probe: Mutex<Lru<(u64, usize, usize), Bytes>>,
    sched: Mutex<Sched>,
    ids: Mutex<HashMap<(u64, usize, usize), u64>>,
    next: AtomicU64,
    max_running: usize,
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
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads.saturating_sub(1).max(1))
            .thread_name(|i| format!("eo-decode-{i}"))
            .build()
            .expect("rayon pool");
        let (tx, rx) = mpsc::channel();
        let inner = Inner {
            rt: rt.handle().clone(),
            max_running: 2 * threads + 16,
            pool,
            jobs: Default::default(),
            limit: ram,
            work: AtomicUsize::new(0),
            raw: Mutex::new(Lru::new(ram / 4)),
            dec: Mutex::new(Lru::new(ram / 2)),
            inflight: Default::default(),
            probe: Mutex::new(Lru::new(ram / 16)),
            sched: Default::default(),
            ids: Default::default(),
            next: AtomicU64::new(1),
            tx,
            wake: Box::new(wake),
        };
        (Engine { inner: Arc::new(inner), rt }, rx)
    }

    /// Open a local path or a URL. The result comes as `Event::Opened` with the returned request id.
    pub fn open(&self, url: String) -> u64 {
        let i = self.inner.clone();
        let req = i.next.fetch_add(1, Relaxed);
        self.rt.spawn_blocking(move || {
            let res = eo_io::open(&url, &i.rt).and_then(|ds| {
                let ds_id = i.next.fetch_add(1, Relaxed);
                i.layer(Arc::new(ds), ds_id, 0, 0)
            });
            i.send(Event::Opened { req, res });
        });
        req
    }

    /// Make a layer for another variable or choice of the dataset of `l`. The result comes as `Event::Opened`.
    pub fn select(&self, l: &Layer, var: usize, choice: usize) -> u64 {
        let (i, ds, ds_id) = (self.inner.clone(), l.ds.clone(), l.ds_id);
        let req = i.next.fetch_add(1, Relaxed);
        self.rt.spawn_blocking(move || {
            let res = i.layer(ds, ds_id, var, choice);
            i.send(Event::Opened { req, res });
        });
        req
    }

    /// Set the tiles that `client` needs, in priority order (the most important first).
    pub fn want(&self, client: u32, tiles: Vec<(Arc<Layer>, TileKey)>) {
        self.inner.sched.lock().unwrap().lists.insert(client, tiles);
        self.inner.dispatch();
    }

    /// Read the values of all bands at level-0 pixel (x, y). The result comes as `Event::Probe`.
    pub fn probe(&self, l: Arc<Layer>, x: u64, y: u64) {
        let i = self.inner.clone();
        self.rt.spawn(async move {
            match i.probe_at(&l, x, y).await {
                Ok(values) => i.send(Event::Probe { layer: l.id, x, y, values }),
                Err(e) => i.send(Event::Error(e.0)),
            }
        });
    }

    pub fn stats(&self) -> Stats {
        let i = &self.inner;
        let s = i.sched.lock().unwrap();
        Stats {
            limit: i.limit,
            raw: i.raw.lock().unwrap().bytes,
            dec: i.dec.lock().unwrap().bytes,
            probe: i.probe.lock().unwrap().bytes,
            work: i.work.load(Relaxed),
            running: s.running.len(),
            wanted: s.lists.values().map(|l| l.len()).sum(),
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

    fn layer(&self, ds: Arc<Dataset>, ds_id: u64, var: usize, choice: usize) -> Result<Arc<Layer>> {
        let v = ds.product.vars.get(var).ok_or("no such variable")?;
        let ((band, part), levels) = (Layer::choice(v, choice), display_levels(v));
        let id = *self.ids.lock().unwrap().entry((ds_id, var, choice)).or_insert_with(|| self.next.fetch_add(1, Relaxed));
        let mut l = Layer {
            id,
            levels,
            ds,
            ds_id,
            var,
            choice,
            band,
            part,
            enc: Enc { u8: false, k: 1.0, off: 0.0 },
            sample: vec![],
        };
        l.ds.sources.iter().for_each(|s| s.sparse(true));
        let raw = self.rt.block_on(self.sample(&l));
        l.ds.sources.iter().for_each(|s| s.sparse(false));
        let raw = raw?;
        l.enc = choose_enc(l.var().levels[0].dtype, part, &raw);
        let v = l.var();
        let (s, o) = if part == Part::Amp || part == Part::Phase { (1.0, 0.0) } else { (v.scale as f32, v.offset as f32) };
        l.sample = raw.into_iter().map(|r| r * s + o).collect();
        l.sample.sort_unstable_by(f32::total_cmp);
        Ok(Arc::new(l))
    }

    /// Sample of raw values: spaced values of all chunks of the coarsest level, or of 16 chunks spread over it.
    async fn sample(&self, l: &Layer) -> Result<Vec<f32>> {
        let v = l.var();
        let a = v.levels.last().unwrap();
        let (ch, cw) = chunk_size(a);
        let (h, w) = (a.len_of("y"), a.len_of("x"));
        let (gy, gx) = (h.div_ceil(ch), w.div_ceil(cw));
        let n = (gy * gx) as usize;
        let pick: Vec<(u64, u64)> = (0..n.min(16)).map(|i| i * n / n.min(16)).map(|i| (i as u64 / gx, i as u64 % gx)).collect();
        let locs: Vec<ChunkLoc> = pick.iter().map(|&(cy, cx)| a.chunks[l.chunk_at(a, cy, cx)]).collect();
        let raws = self.raw_many(l, &locs).await?;
        let (a, fill, part, band) = (a.clone(), v.fill, l.part, l.band);
        self.on_pool(0, move || {
            let p = PlaneAt::new(&a, band);
            let per = 65_536 / pick.len().max(1);
            let parts: Vec<Result<Vec<f32>>> = pick
                .par_iter()
                .zip(&raws)
                .map(|(&(cy, cx), raw)| {
                    if raw.is_empty() {
                        return Ok(vec![]);
                    }
                    let d = readable(&a, raw)?;
                    // Only the pixels inside the array: edge chunks have padding.
                    let (rows, cols) = ((h - cy * ch).min(ch) as usize, (w - cx * cw).min(cw) as usize);
                    // Up to 64 spaced rows, 16 short runs of adjacent values in each row: few memory pages
                    // of a large uncompressed chunk. The rows go to all threads: page faults wait in parallel.
                    let nr = rows.min(64);
                    let run = (per / (nr * 16)).clamp(1, cols.div_ceil(16));
                    let (d, a) = (&d, &a);
                    let vals: Vec<Vec<f32>> = (0..nr)
                        .into_par_iter()
                        .map(|k| {
                            let r = k * rows / nr;
                            let mut out = Vec::with_capacity(16 * run);
                            for s in 0..16 {
                                let c0 = s * cols / 16;
                                let win = Win { r0: r, r1: r + 1, c0, c1: (c0 + run).min(cols) };
                                if win.w() == 0 || !p.covers(a, d.len(), &win) {
                                    continue;
                                }
                                for c in win.c0..win.c1 {
                                    let x = value_f64(a, d, p.base + r * p.sy + c * p.sx, part);
                                    if x.is_finite() && Some(x) != fill {
                                        out.push(x as f32);
                                    }
                                }
                            }
                            out
                        })
                        .collect();
                    Ok(vals.concat())
                })
                .collect();
            let mut out = vec![];
            for p in parts {
                out.extend(p?);
            }
            out.sort_unstable_by(f32::total_cmp);
            Ok(out)
        })
        .await
    }

    /// Encoded bytes of chunks. Local data is not copied. Remote data goes through the raw byte cache.
    async fn raw_many(&self, l: &Layer, locs: &[ChunkLoc]) -> Result<Vec<Bytes>> {
        let mut out = vec![Bytes::new(); locs.len()];
        let mut miss: HashMap<u32, Vec<usize>> = HashMap::new();
        {
            let mut raw = self.raw.lock().unwrap();
            for (i, c) in locs.iter().enumerate() {
                let s = l.ds.sources.get(c.src as usize).ok_or("bad source index")?;
                if c.len == 0 {
                    continue;
                } else if s.is_local() {
                    out[i] = s.read(c.off..c.off + c.len)?;
                } else if let Some(b) = raw.get(&(l.ds_id, c.src, c.off)) {
                    out[i] = b.clone();
                } else {
                    miss.entry(c.src).or_default().push(i);
                }
            }
        }
        for (src, idx) in miss {
            let ranges: Vec<_> = idx.iter().map(|&i| locs[i].off..locs[i].off + locs[i].len).collect();
            let got = l.ds.sources[src as usize].get_ranges(&ranges).await?;
            let mut raw = self.raw.lock().unwrap();
            for (&i, b) in idx.iter().zip(got) {
                raw.insert((l.ds_id, src, locs[i].off), b.clone(), b.len());
                out[i] = b;
            }
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
                } else if let Some((b, j)) = inf.get(&k) {
                    waits.push((i, b.clone(), *j));
                } else {
                    claim.push((i, idx));
                }
            }
            if !claim.is_empty() {
                let ci: Vec<usize> = claim.iter().map(|c| c.1).collect();
                let b = self.clone().batch(l.clone(), lvl, ci, prio).boxed().shared();
                for (j, &(i, idx)) in claim.iter().enumerate() {
                    inf.insert(DecKey::Chunk { layer: l.id, lvl, idx }, (b.clone(), j));
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
        let locs: Vec<ChunkLoc> = idxs.iter().map(|&i| a.chunks[i]).collect();
        let res = match self.raw_many(&l, &locs).await {
            Ok(raws) => {
                let l2 = l.clone();
                let _c = Charge::new(&self.work, a.chunk_bytes() * raws.len());
                self.on_pool(prio, move || {
                    let a = &l2.var().levels[lvl];
                    let p = PlaneAt::new(a, l2.band);
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
    }

    async fn tile(self: &Arc<Self>, l: &Arc<Layer>, key: TileKey, prio: u32) -> Result<()> {
        let lv = l.levels.get(key.lv as usize).ok_or("bad level")?;
        let (x0, y0) = (key.tx as u64 * TILE, key.ty as u64 * TILE);
        if x0 >= lv.w || y0 >= lv.h {
            return Err("tile outside the image".into());
        }
        let (w, h) = (TILE.min(lv.w - x0), TILE.min(lv.h - y0));
        let send = |px, done| self.send(Event::Tile { key, w: w as u32, h: h as u32, px, done });
        match lv.src {
            LevelSrc::File(lvl) => {
                let a = &l.var().levels[lvl];
                let (ch, cw) = chunk_size(a);
                let cells: Vec<(u64, u64)> =
                    (y0 / ch..=(y0 + h - 1) / ch).flat_map(|cy| (x0 / cw..=(x0 + w - 1) / cw).map(move |cx| (cy, cx))).collect();
                let idxs: Vec<usize> = cells.iter().map(|&(cy, cx)| l.chunk_at(a, cy, cx)).collect();
                if a.codecs.is_empty() {
                    // Uncompressed: read the tile region from the chunk bytes. The memory map is the cache.
                    let locs: Vec<ChunkLoc> = idxs.iter().map(|&i| a.chunks[i]).collect();
                    let raws = self.raw_many(l, &locs).await?;
                    let l2 = l.clone();
                    let t = self
                        .on_pool(prio, move || {
                            let a = &l2.var().levels[lvl];
                            let p = PlaneAt::new(a, l2.band);
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
                let px = self.generate(l, base, f, (x0, y0, w, h), prio, &send).await?;
                self.dec.lock().unwrap().insert(tk, px.clone(), px.size());
                send(px, true);
            }
        }
        Ok(())
    }

    /// Make an overview tile: the `f` x `f` mean of display level `base`. Send partial tiles while the chunks arrive.
    /// `t` is the tile rectangle (x, y, width, height) at the overview level.
    async fn generate(
        self: &Arc<Self>,
        l: &Arc<Layer>,
        base: usize,
        f: u64,
        t: (u64, u64, u64, u64),
        prio: u32,
        send: &impl Fn(Arc<Pixels>, bool),
    ) -> Result<Arc<Pixels>> {
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
                    let locs: Vec<ChunkLoc> = g.iter().map(|u| a.chunks[l.chunk_at(a, u.cy, u.cx)]).collect();
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
        Ok(Arc::new(mean(l, &sum, &cnt)))
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
            let idx = chunk_at(a, band, y / ch, x / cw);
            let key = (l.ds_id, l.var, idx);
            let cached = self.probe.lock().unwrap().get(&key).cloned();
            let d = match cached {
                Some(d) => d,
                None => {
                    let raw = self.raw_many(l, &[a.chunks[idx]]).await?.remove(0);
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
            let p = PlaneAt::new(a, band);
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
    let p = PlaneAt::new(a, l.band);
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
            chunks: vec![],
        };
        Variable {
            name: "t".into(),
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
