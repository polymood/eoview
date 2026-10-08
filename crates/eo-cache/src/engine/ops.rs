//! Computed layers: layers that have no file. Their tiles are the result of an operation on the tiles of
//! other layers (see DESIGN.md, section 3). The view does not know that a layer is computed: the
//! priority, the cancel and the caches are the same as for a layer of a file.
//!
//! The operations: an aggregate over time (mean, median, minimum, maximum, standard deviation, number of
//! values) of the time steps of a variable, a function of the values of layers on the same grid (layer
//! math, mask), and the values of an array (the output of a Python script). The engine reads the values of the
//! inputs (f32, not the 16-bit display tiles) at the level of the tile: the mean of a coarse level is
//! near the mean of the level 0, and equal if the overviews of the files are means.
use super::*;

/// An aggregate over time.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Agg {
    Mean,
    Median,
    Min,
    Max,
    Std,
    Count,
}

impl Agg {
    pub const ALL: [(Agg, &str); 6] = [(Agg::Mean, "Mean"), (Agg::Median, "Median"), (Agg::Min, "Minimum"), (Agg::Max, "Maximum"), (Agg::Std, "Standard deviation"), (Agg::Count, "Number of values")];

    pub fn name(self) -> &'static str {
        Agg::ALL.iter().find(|a| a.0 == self).map_or("", |a| a.1)
    }

    /// The aggregate of `v` (NaN: no data). The result is NaN if no value has data (0 for the count).
    pub fn of(self, v: &mut [f32]) -> f32 {
        let mut n = 0;
        for i in 0..v.len() {
            if v[i].is_finite() {
                v[n] = v[i];
                n += 1;
            }
        }
        let v = &mut v[..n];
        if n == 0 {
            return if self == Agg::Count { 0.0 } else { f32::NAN };
        }
        match self {
            Agg::Count => n as f32,
            Agg::Min => v.iter().copied().fold(f32::INFINITY, f32::min),
            Agg::Max => v.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            Agg::Mean => (v.iter().map(|&x| x as f64).sum::<f64>() / n as f64) as f32,
            Agg::Std => {
                let m = v.iter().map(|&x| x as f64).sum::<f64>() / n as f64;
                (v.iter().map(|&x| (x as f64 - m).powi(2)).sum::<f64>() / n as f64).sqrt() as f32
            }
            Agg::Median => {
                v.sort_unstable_by(f32::total_cmp);
                if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
            }
        }
    }
}

/// A time step of an aggregate: a step of the time dimension of the first layer, a product (the same
/// variable, at its first step), or a layer that is ready.
pub enum StepIn {
    Time(u64),
    Path(String),
    Layer(Arc<Layer>),
}

/// The operation of a computed layer.
pub struct Op {
    pub kind: OpKind,
    /// The same for the same operation on the same data in all sessions: the key of the disk cache.
    key: String,
}

/// A function of the values of the inputs at one pixel (NaN: no data).
pub type PixelFn = Arc<dyn Fn(&[f64]) -> f64 + Send + Sync>;

pub enum OpKind {
    /// An aggregate over time of the layers of the time steps.
    Agg { how: Agg, inputs: Vec<Arc<Layer>> },
    /// A function of the values of the inputs at the same pixel. The inputs are on the same grid.
    Math { f: PixelFn, inputs: Vec<Arc<Layer>> },
    /// Values in memory (the output of a Python script): `w` columns, NaN for no data.
    Array { w: u64, data: Arc<Vec<f32>> },
}

/// The sums of the values of a tile, for the aggregates that do not keep all values.
enum Acc {
    Sum { sum: Vec<f64>, sum2: Option<Vec<f64>>, cnt: Vec<u32> },
    Ext { v: Vec<f32>, max: bool },
    All(Vec<Vec<f32>>),
}

impl Acc {
    fn new(how: Agg, n: usize) -> Acc {
        match how {
            Agg::Min | Agg::Max => Acc::Ext { v: vec![f32::NAN; n], max: how == Agg::Max },
            Agg::Median => Acc::All(vec![]),
            _ => Acc::Sum { sum: vec![0.0; n], sum2: (how == Agg::Std).then(|| vec![0.0; n]), cnt: vec![0; n] },
        }
    }

    fn add(&mut self, x: Vec<f32>) {
        match self {
            Acc::Sum { sum, sum2, cnt } => {
                for (i, &v) in x.iter().enumerate().filter(|v| v.1.is_finite()) {
                    sum[i] += v as f64;
                    cnt[i] += 1;
                    if let Some(s2) = sum2 {
                        s2[i] += v as f64 * v as f64;
                    }
                }
            }
            Acc::Ext { v, max } => {
                for (a, &b) in v.iter_mut().zip(&x) {
                    // NaN is no data: f32::min and f32::max keep the other value.
                    *a = if *max { a.max(b) } else { a.min(b) };
                }
            }
            Acc::All(all) => all.push(x),
        }
    }

    fn result(&self, how: Agg) -> Vec<f32> {
        match self {
            Acc::Sum { sum, sum2, cnt } => (0..cnt.len())
                .map(|i| {
                    let n = cnt[i] as f64;
                    match (how, sum2) {
                        (Agg::Count, _) => cnt[i] as f32,
                        _ if cnt[i] == 0 => f32::NAN,
                        (Agg::Std, Some(s2)) => (s2[i] / n - (sum[i] / n).powi(2)).max(0.0).sqrt() as f32,
                        _ => (sum[i] / n) as f32,
                    }
                })
                .collect(),
            Acc::Ext { v, .. } => v.clone(),
            Acc::All(all) => {
                let n = all.first().map_or(0, |a| a.len());
                let mut col = vec![0f32; all.len()];
                (0..n)
                    .map(|i| {
                        col.iter_mut().zip(all).for_each(|(c, a)| *c = a[i]);
                        how.of(&mut col)
                    })
                    .collect()
            }
        }
    }
}

/// The array of a computed variable: no time and no band dimension, 32-bit floats. The engine does not
/// read its chunks.
fn flat(a: &Array) -> Array {
    let keep: Vec<usize> = (0..a.dims.len()).filter(|&i| a.dims[i] != "time" && a.dims[i] != "band").collect();
    Array {
        dims: keep.iter().map(|&i| a.dims[i].clone()).collect(),
        shape: keep.iter().map(|&i| a.shape[i]).collect(),
        chunk: keep.iter().map(|&i| a.chunk[i]).collect(),
        dtype: DType::F32,
        codecs: vec![],
        ..a.clone()
    }
}

/// The physical value of a stored value of layer `l`: (scale, offset).
fn phys(l: &Layer) -> (f32, f32) {
    let v = l.var();
    if matches!(l.part, Part::Amp | Part::Phase) { (1.0, 0.0) } else { (v.scale as f32, v.offset as f32) }
}

impl Engine {
    /// Make a layer that is the aggregate `how` of the time steps `steps` of variable `var` (and its choice)
    /// of the product of `base`. All steps must be on the same grid. The result comes as `Event::Opened`.
    pub fn aggregate(&self, base: Arc<Layer>, var: usize, choice: usize, steps: Vec<StepIn>, how: Agg) -> u64 {
        let i = self.inner.clone();
        let req = i.next.fetch_add(1, Relaxed);
        self.rt.spawn_blocking(move || {
            let res = i.make_aggregate(&base, var, choice, steps, how);
            i.send(Event::Opened { req, res });
        });
        req
    }
}

impl Inner {
    fn make_aggregate(&self, base: &Arc<Layer>, var: usize, choice: usize, steps: Vec<StepIn>, how: Agg) -> Result<Arc<Layer>> {
        if steps.is_empty() {
            return Err("no time step".into());
        }
        let name = base.ds.product.vars.get(var).ok_or("no such variable")?.name.clone();
        // The products open in parallel: the latency of remote products adds up otherwise.
        let open = |s: &StepIn| -> Result<Arc<Layer>> {
            match s {
                StepIn::Layer(l) => Ok(l.clone()),
                StepIn::Time(t) => self.layer(base.ds.clone(), base.ds_id, var, choice, *t),
                StepIn::Path(p) => {
                    let ds = eo_io::open(p, &self.rt)?;
                    let var = ds.product.vars.iter().position(|v| v.name == name).ok_or_else(|| Error(format!("{p}: no variable {name}")))?;
                    let ds_id = self.next.fetch_add(1, Relaxed);
                    self.layer(Arc::new(ds), ds_id, var, choice, 0)
                }
            }
        };
        let mut inputs = vec![];
        for part in steps.chunks(8) {
            let got: Vec<Result<Arc<Layer>>> = std::thread::scope(|sc| part.iter().map(|s| sc.spawn(move || open(s))).collect::<Vec<_>>().into_iter().map(|h| h.join().unwrap_or_else(|_| Err("open stopped".into()))).collect());
            for g in got {
                inputs.push(g?);
            }
        }
        let g0 = self.same_grid(&inputs, "time steps")?;
        if inputs.iter().any(|l| l.op.is_some()) {
            return Err("an aggregate of computed layers is not possible in this version".into());
        }
        // The sample: the aggregate of the samples of the steps. They have their values at the same pixels.
        let len = inputs.iter().map(|l| l.sample_at.len()).min().unwrap_or(0);
        let mut col = vec![0f32; inputs.len()];
        let sample_at: Vec<f32> = (0..len)
            .map(|k| {
                col.iter_mut().zip(&inputs).for_each(|(c, l)| *c = l.sample_at[k]);
                how.of(&mut col)
            })
            .collect();
        let mut sample: Vec<f32> = sample_at.iter().copied().filter(|v| v.is_finite()).collect();
        sample.sort_unstable_by(f32::total_cmp);
        let v0 = inputs[0].var();
        let var = Variable {
            name: format!("{name} {}", how.name().to_lowercase()),
            group: String::new(),
            levels: v0.levels.iter().map(flat).collect(),
            bands: vec![format!("{name} {}", how.name().to_lowercase())],
            fill: None,
            scale: 1.0,
            offset: 0.0,
            units: if how == Agg::Count { String::new() } else { v0.units.clone() },
            georef: g0,
            times: Default::default(),
        };
        let desc = format!("{} of {} time steps of {name}", how.name(), inputs.len());
        let product = Product { name: format!("{name} {}", how.name().to_lowercase()), desc, vars: vec![var], valid: None };
        let ds = Arc::new(Dataset { product, sources: inputs[0].ds.sources.clone() });
        let src = |l: &Layer| l.var().levels[0].chunks.iter().next().and_then(|c| l.ds.sources.get(c.src as usize)).map_or(String::new(), |s| s.name().to_string());
        let key = format!("{how:?} {:?}", inputs.iter().map(|l| (src(l), l.var().name.clone(), l.choice, l.time)).collect::<Vec<_>>());
        let enc = choose_enc(DType::F32, Part::Real, &sample);
        let levels = inputs[0].levels.clone();
        let op = Op { kind: OpKind::Agg { how, inputs }, key };
        let l = Layer {
            id: self.next.fetch_add(1, Relaxed),
            ds,
            ds_id: self.next.fetch_add(1, Relaxed),
            var: 0,
            choice: 0,
            band: 0,
            time: 0,
            part: Part::Real,
            levels,
            enc,
            sample,
            sample_at,
            op: Some(Arc::new(op)),
        };
        Ok(Arc::new(l))
    }

    /// The georeferencing of `inputs`: they must be on the same grid (`what`: their name in the message).
    fn same_grid(&self, inputs: &[Arc<Layer>], what: &str) -> Result<Georef> {
        let g0 = self.georef(&inputs[0])?;
        for l in &inputs[1..] {
            if l.size() != inputs[0].size() || l.levels.len() != inputs[0].levels.len() || self.georef(l)? != g0 {
                return Err(Error(format!("the {what} are not on the same grid: {} is not on the grid of {}", l.ds.product.name, inputs[0].ds.product.name)));
            }
        }
        Ok(g0)
    }

    /// A layer with the values `f` of the values of `inputs` (on the same grid) at each pixel.
    fn make_math(&self, name: &str, units: &str, inputs: Vec<Arc<Layer>>, f: PixelFn, key: String) -> Result<Arc<Layer>> {
        if inputs.is_empty() {
            return Err("no input layer".into());
        }
        let g = self.same_grid(&inputs, "input layers")?;
        let len = inputs.iter().map(|l| l.sample_at.len()).min().unwrap_or(0);
        let mut v = vec![0f64; inputs.len()];
        let sample_at: Vec<f32> = (0..len)
            .map(|k| {
                v.iter_mut().zip(&inputs).for_each(|(x, l)| *x = l.sample_at[k] as f64);
                f(&v) as f32
            })
            .collect();
        let mut sample: Vec<f32> = sample_at.iter().copied().filter(|v| v.is_finite()).collect();
        sample.sort_unstable_by(f32::total_cmp);
        let var = Variable {
            name: name.into(),
            group: String::new(),
            levels: inputs[0].var().levels.iter().map(flat).collect(),
            bands: vec![name.into()],
            fill: None,
            scale: 1.0,
            offset: 0.0,
            units: units.into(),
            georef: g,
            times: Default::default(),
        };
        let product = Product { name: name.into(), desc: format!("{name}: a function of {} layers", inputs.len()), vars: vec![var], valid: None };
        let ds = Arc::new(Dataset { product, sources: inputs[0].ds.sources.clone() });
        let (levels, enc) = (inputs[0].levels.clone(), choose_enc(DType::F32, Part::Real, &sample));
        let op = Op { kind: OpKind::Math { f, inputs }, key };
        let id = self.next.fetch_add(1, Relaxed);
        Ok(Arc::new(Layer { id, ds, ds_id: self.next.fetch_add(1, Relaxed), var: 0, choice: 0, band: 0, time: 0, part: Part::Real, levels, enc, sample, sample_at, op: Some(Arc::new(op)) }))
    }

    /// The physical values (NaN: no data) of tile (tx, ty) of display level `lv` of layer `l`.
    fn values<'a>(self: &'a Arc<Self>, l: &'a Arc<Layer>, lv: usize, tx: u32, ty: u32, prio: u32) -> BoxFuture<'a, Result<Vec<f32>>> {
        async move {
            match &l.op {
                Some(op) => self.op_values(l, op, TileKey { layer: l.id, lv: lv as u8, tx, ty }, prio, None).await,
                None => self.file_values(l, lv, tx, ty, prio).await,
            }
        }
        .boxed()
    }

    /// `values` of a layer of a file.
    async fn file_values(self: &Arc<Self>, l: &Arc<Layer>, lv: usize, tx: u32, ty: u32, prio: u32) -> Result<Vec<f32>> {
        let level = l.levels.get(lv).ok_or("bad level")?;
        let (x0, y0) = (tx as u64 * TILE, ty as u64 * TILE);
        let (w, h) = (TILE.min(level.w - x0), TILE.min(level.h - y0));
        let (s, o) = phys(l);
        let fill = l.var().fill.map(|f| f as f32);
        match level.src {
            LevelSrc::File(lvl) => {
                let a = &l.var().levels[lvl];
                let (ch, cw) = chunk_size(a);
                let cells: Vec<(u64, u64)> = (y0 / ch..=(y0 + h - 1) / ch).flat_map(|cy| (x0 / cw..=(x0 + w - 1) / cw).map(move |cx| (cy, cx))).collect();
                let locs: Vec<ChunkLoc> = cells.iter().map(|&(cy, cx)| a.chunks.at(l.chunk_at(a, cy, cx))).collect();
                let raws = self.raw_many(l, &locs).await?;
                let l2 = l.clone();
                self.on_pool(prio, move || {
                    let a = &l2.var().levels[lvl];
                    let p = PlaneAt::new(a, l2.band, l2.time);
                    let mut out = vec![f32::NAN; (w * h) as usize];
                    let mut row = vec![0f32; cw as usize];
                    for (&(cy, cx), raw) in cells.iter().zip(&raws) {
                        if raw.is_empty() {
                            continue;
                        }
                        let d = readable(a, raw)?;
                        let (ry0, ry1) = ((cy * ch).max(y0), ((cy + 1) * ch).min(y0 + h));
                        let (rx0, rx1) = ((cx * cw).max(x0), ((cx + 1) * cw).min(x0 + w));
                        let win = Win { r0: (ry0 - cy * ch) as usize, r1: (ry1 - cy * ch) as usize, c0: (rx0 - cx * cw) as usize, c1: (rx1 - cx * cw) as usize };
                        if !p.covers(a, d.len(), &win) {
                            return Err(Error("chunk data is shorter than the chunk".into()));
                        }
                        for y in ry0..ry1 {
                            let wr = Win { r0: (y - cy * ch) as usize, r1: (y - cy * ch + 1) as usize, ..win };
                            let row = &mut row[..win.w()];
                            to_f32(a, &d, &p, l2.part, &wr, row);
                            let d0 = ((y - y0) * w + rx0 - x0) as usize;
                            for (o2, &v) in out[d0..d0 + row.len()].iter_mut().zip(row.iter()) {
                                *o2 = if v.is_finite() && Some(v) != fill { v * s + o } else { f32::NAN };
                            }
                        }
                    }
                    Ok(out)
                })
                .await
            }
            LevelSrc::Virtual { base, f } => {
                let (sum, cnt) = self.generate(l, base, f, (x0, y0, w, h), prio, &|_, _| {}).await?;
                Ok(sum.iter().zip(&cnt).map(|(&v, &c)| if c == 0 { f32::NAN } else { (v / c as f64) as f32 * s + o }).collect())
            }
        }
    }

    /// Make tile `key` of the computed layer `l`. The partial tiles show the result of the steps that are read.
    pub(super) async fn op_tile(self: &Arc<Self>, l: &Arc<Layer>, op: &Arc<Op>, key: TileKey, prio: u32, send: &(impl Fn(Arc<Pixels>, bool) + Sync)) -> Result<Arc<Pixels>> {
        let tk = DecKey::Tile(key);
        if let Some(p) = self.dec.lock().unwrap().get(&tk).cloned() {
            return Ok(p);
        }
        // A result with remote inputs stays on the disk: the next session does not read the steps again.
        let remote = matches!(&op.kind, OpKind::Agg { inputs, .. } if inputs.iter().any(|i| !i.is_local()));
        let disk = self.disk.get().filter(|_| remote).cloned();
        let lv = &l.levels[key.lv as usize];
        let n = (TILE.min(lv.w - key.tx as u64 * TILE) * TILE.min(lv.h - key.ty as u64 * TILE)) as usize;
        let dkey = (op.key.clone(), key.lv, key.tx, key.ty, l.enc.k.to_bits(), l.enc.off.to_bits());
        if let Some(d) = disk.clone() {
            let k = dkey.clone();
            let read = move || d.get(&k).and_then(|b| Pixels::from_bytes(false, &b, n));
            if let Some(px) = tokio::task::spawn_blocking(read).await.ok().flatten() {
                let px = Arc::new(px);
                self.dec.lock().unwrap().insert(tk, px.clone(), px.size());
                return Ok(px);
            }
        }
        let enc = |mut f: Vec<f32>| Arc::new(Pixels::F16(encode_f32(&mut f, &l.enc, None)));
        let partial = |f: Vec<f32>| send(enc(f), false);
        let px = enc(self.op_values(l, op, key, prio, Some(&partial)).await?);
        self.dec.lock().unwrap().insert(tk, px.clone(), px.size());
        if let Some(d) = disk {
            let p = px.clone();
            self.rt.spawn_blocking(move || d.put(&dkey, p.bytes()));
        }
        Ok(px)
    }

    /// The values of tile `key` of the computed layer `l`. `partial`: a function for the results before the
    /// end (the steps that are read).
    async fn op_values(self: &Arc<Self>, l: &Arc<Layer>, op: &Op, key: TileKey, prio: u32, partial: Option<&(dyn Fn(Vec<f32>) + Sync)>) -> Result<Vec<f32>> {
        let lv = l.levels[key.lv as usize];
        let (x0, y0) = (key.tx as u64 * TILE, key.ty as u64 * TILE);
        let (tw, th) = (TILE.min(lv.w - x0), TILE.min(lv.h - y0));
        let n = (tw * th) as usize;
        match &op.kind {
            OpKind::Array { w, data } => {
                // The mean of the values of the array in each pixel of the level.
                let (w, data) = (*w, data.clone());
                let h = data.len() as u64 / w;
                Ok(self
                    .on_pool(prio, move || {
                        let mut out = vec![f32::NAN; n];
                        for j in 0..th {
                            let (r0, r1) = (((y0 + j) as f64 * lv.ky + lv.oy) as u64, ((((y0 + j + 1) as f64 * lv.ky + lv.oy) as u64).max(1)).min(h));
                            for i in 0..tw {
                                let (c0, c1) = (((x0 + i) as f64 * lv.kx + lv.ox) as u64, ((((x0 + i + 1) as f64 * lv.kx + lv.ox) as u64).max(1)).min(w));
                                let (mut s, mut c) = (0f64, 0u32);
                                for r in r0..r1.max(r0 + 1).min(h) {
                                    for v in &data[(r * w + c0) as usize..(r * w + c1.max(c0 + 1).min(w)) as usize] {
                                        if v.is_finite() {
                                            s += *v as f64;
                                            c += 1;
                                        }
                                    }
                                }
                                if c > 0 {
                                    out[(j * tw + i) as usize] = (s / c as f64) as f32;
                                }
                            }
                        }
                        out
                    })
                    .await)
            }
            OpKind::Math { f, inputs } => {
                let vals: Vec<Vec<f32>> = stream::iter(inputs.clone())
                    .map(|inp| {
                        let i = self.clone();
                        async move { i.values(&inp, key.lv as usize, key.tx, key.ty, prio).await }
                    })
                    .buffered(4)
                    .collect::<Vec<_>>()
                    .await
                    .into_iter()
                    .collect::<Result<_>>()?;
                if vals.iter().any(|v| v.len() != n) {
                    return Err("the input layers are not on the same grid".into());
                }
                let f = f.clone();
                Ok(self
                    .on_pool(prio, move || {
                        let mut v = vec![0f64; vals.len()];
                        (0..n)
                            .map(|i| {
                                v.iter_mut().zip(&vals).for_each(|(x, a)| *x = a[i] as f64);
                                f(&v) as f32
                            })
                            .collect()
                    })
                    .await)
            }
            OpKind::Agg { how, inputs } => {
                let how = *how;
                // The median keeps the values of all steps.
                if how == Agg::Median && n * 4 * inputs.len() > self.limit / 4 {
                    return Err(Error(format!("a median of {} steps needs {} MB for one tile: more than a quarter of the memory budget. Use fewer steps", inputs.len(), (n * 4 * inputs.len()) >> 20)));
                }
                let _slot = self.ops.acquire().await.map_err(|_| Error("engine stopped".into()))?;
                let _c = Charge::new(&self.work, n * if how == Agg::Median { 4 * inputs.len() } else { 16 });
                let mut acc = Acc::new(how, n);
                let mut jobs = stream::iter(inputs.clone())
                    .map(|inp| {
                        let i = self.clone();
                        async move { i.values(&inp, key.lv as usize, key.tx, key.ty, prio).await }
                    })
                    .buffer_unordered(4);
                let mut last = Instant::now();
                while let Some(v) = jobs.next().await {
                    let v = v?;
                    if v.len() != n {
                        return Err("the time steps are not on the same grid".into());
                    }
                    acc.add(v);
                    if let Some(p) = partial.filter(|_| how != Agg::Median && last.elapsed() > PARTIAL) {
                        p(acc.result(how));
                        last = Instant::now();
                    }
                }
                Ok(acc.result(how))
            }
        }
    }

    /// The value of the computed layer `l` at level-0 pixel (x, y): the aggregate of the exact values of its
    /// inputs, or the value of the array.
    pub(super) async fn probe_op(&self, op: &Op, x: u64, y: u64) -> Result<Vec<Option<f64>>> {
        let r = match &op.kind {
            OpKind::Array { w, data } => data.get((y * w + x) as usize).copied().unwrap_or(f32::NAN),
            OpKind::Agg { how, inputs } => {
                let mut v = vec![];
                for inp in inputs {
                    v.push(self.probe_one(inp, x, y).await? as f32);
                }
                how.of(&mut v)
            }
            OpKind::Math { f, inputs } => {
                let mut v = vec![];
                for inp in inputs {
                    v.push(self.probe_one(inp, x, y).await?);
                }
                f(&v) as f32
            }
        };
        Ok(vec![r.is_finite().then_some(r as f64)])
    }

    /// The exact value of layer `l` (a file or computed) at level-0 pixel (x, y). NaN: no data.
    fn probe_one<'a>(&'a self, l: &'a Layer, x: u64, y: u64) -> BoxFuture<'a, Result<f64>> {
        async move {
            let v = match &l.op {
                Some(op) => self.probe_op(op, x, y).await?.first().copied().flatten(),
                None => self.probe_at(l, x, y).await?.get(l.band as usize).copied().flatten(),
            };
            Ok(v.unwrap_or(f64::NAN))
        }
        .boxed()
    }

    /// The values of window (x0, y0, w, h) of level `lv` of layer `l`, and its georeferencing.
    async fn read(self: &Arc<Self>, l: &Arc<Layer>, lv: usize, win: (u64, u64, u64, u64)) -> Result<(Vec<f32>, Georef)> {
        let level = *l.levels.get(lv).ok_or("bad level")?;
        let (x0, y0, w, h) = win;
        if w == 0 || h == 0 || x0 + w > level.w || y0 + h > level.h {
            return Err("the window is not in the layer".into());
        }
        if w * h * 4 > (self.limit / 4) as u64 {
            return Err(Error(format!("{w} x {h} values need {} MB: more than a quarter of the memory budget. Use a coarser level or a smaller area", (w * h * 4) >> 20)));
        }
        let _c = Charge::new(&self.work, (w * h * 4) as usize);
        let mut out = vec![f32::NAN; (w * h) as usize];
        let tiles: Vec<(u32, u32)> = (y0 / TILE..=(y0 + h - 1) / TILE).flat_map(|ty| (x0 / TILE..=(x0 + w - 1) / TILE).map(move |tx| (tx as u32, ty as u32))).collect();
        let mut jobs = stream::iter(tiles)
            .map(|(tx, ty)| {
                let i = self.clone();
                async move { i.values(l, lv, tx, ty, 0).await.map(|v| (tx, ty, v)) }
            })
            .buffer_unordered(4);
        while let Some(r) = jobs.next().await {
            let (tx, ty, v) = r?;
            let (tx0, ty0) = (tx as u64 * TILE, ty as u64 * TILE);
            let tw = TILE.min(level.w - tx0);
            for y in ty0.max(y0)..(ty0 + TILE).min(y0 + h).min(level.h) {
                for x in tx0.max(x0)..(tx0 + tw).min(x0 + w) {
                    out[((y - y0) * w + x - x0) as usize] = v[((y - ty0) * tw + x - tx0) as usize];
                }
            }
        }
        let g = self.georef(l)?.window(level.ox + x0 as f64 * level.kx, level.oy + y0 as f64 * level.ky, level.kx, level.ky);
        Ok((out, g))
    }

    /// A computed layer with the values `data` (`w` columns, NaN: no data) and the georeferencing `g`.
    fn memory_layer(&self, name: &str, units: &str, w: u64, data: Vec<f32>, g: Georef) -> Result<Arc<Layer>> {
        let h = data.len() as u64 / w.max(1);
        if w == 0 || h == 0 || w * h != data.len() as u64 {
            return Err("the size of the array is not correct".into());
        }
        let c = TILE.min(w).max(1);
        let a = Array { dims: vec!["y".into(), "x".into()], shape: vec![h, w], chunk: vec![TILE.min(h), c], dtype: DType::F32, le: true, codecs: vec![], chunks: vec![].into(), place: None };
        let var = Variable { name: name.into(), group: String::new(), levels: vec![a], bands: vec![name.into()], fill: None, scale: 1.0, offset: 0.0, units: units.into(), georef: g, times: Default::default() };
        let levels = display_levels(&var);
        // The sample: 64 K values spread on the array.
        let step = (data.len() / 65_536).max(1);
        let sample_at: Vec<f32> = data.iter().step_by(step).copied().collect();
        let mut sample: Vec<f32> = sample_at.iter().copied().filter(|v| v.is_finite()).collect();
        sample.sort_unstable_by(f32::total_cmp);
        let enc = choose_enc(DType::F32, Part::Real, &sample);
        let product = Product { name: name.into(), desc: "Output of a Python script".into(), vars: vec![var], valid: None };
        let id = self.next.fetch_add(1, Relaxed);
        let op = Op { kind: OpKind::Array { w, data: Arc::new(data) }, key: format!("array {id}") };
        let ds = Arc::new(Dataset { product, sources: vec![] });
        Ok(Arc::new(Layer { id, ds, ds_id: self.next.fetch_add(1, Relaxed), var: 0, choice: 0, band: 0, time: 0, part: Part::Real, levels, enc, sample, sample_at, op: Some(Arc::new(op)) }))
    }
}

impl Engine {
    /// Read the values (NaN: no data) of window (x0, y0, width, height) of display level `lv` of layer `l`.
    /// The result comes as `Event::Read`, with the georeferencing of the window.
    pub fn read(&self, l: Arc<Layer>, lv: usize, win: (u64, u64, u64, u64)) -> u64 {
        let i = self.inner.clone();
        let req = i.next.fetch_add(1, Relaxed);
        self.rt.spawn(async move {
            let res = i.read(&l, lv, win).await.map(|(v, g)| (Arc::new(v), g));
            i.send(Event::Read { req, res });
        });
        req
    }

    /// Make a layer with the values `f` of the values of `inputs` (on the same grid) at each pixel. `key`
    /// names the function and its inputs. The result comes as `Event::Opened`.
    pub fn math(&self, name: String, units: String, inputs: Vec<Arc<Layer>>, f: PixelFn, key: String) -> u64 {
        let i = self.inner.clone();
        let req = i.next.fetch_add(1, Relaxed);
        self.rt.spawn_blocking(move || {
            let res = i.make_math(&name, &units, inputs, f, key);
            i.send(Event::Opened { req, res });
        });
        req
    }

    /// Make a layer of the values `data` (`w` columns, NaN: no data). The result comes as `Event::Opened`.
    pub fn memory(&self, name: String, units: String, w: u64, data: Vec<f32>, g: Georef) -> u64 {
        let i = self.inner.clone();
        let req = i.next.fetch_add(1, Relaxed);
        self.rt.spawn_blocking(move || {
            let res = i.memory_layer(&name, &units, w, data, g);
            i.send(Event::Opened { req, res });
        });
        req
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mean of the 3 steps of a cube (h5py: 289.0625 at column 11, row 7, 284.9943 for all pixels): the
    /// tile (16-bit floats) is near it, the inspector is exact.
    #[test]
    fn mean_of_a_cube() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/time_grid.nc");
        let (e, rx) = Engine::new(64 << 20, || {});
        let next = |f: &dyn Fn(&Event) -> bool| loop {
            let ev = rx.recv_timeout(Duration::from_secs(20)).expect("timeout");
            if f(&ev) {
                break ev;
            }
        };
        e.open(path.into());
        let Event::Opened { res, .. } = next(&|e| matches!(e, Event::Opened { .. })) else { unreachable!() };
        let first = res.unwrap();
        e.aggregate(first, 0, 0, (0..3).map(StepIn::Time).collect(), Agg::Mean);
        let Event::Opened { res, .. } = next(&|e| matches!(e, Event::Opened { .. })) else { unreachable!() };
        let l = res.unwrap();
        assert_eq!((l.size(), l.var().steps()), ((60, 40), 1));
        e.want(1, vec![(l.clone(), TileKey { layer: l.id, lv: 0, tx: 0, ty: 0 })]);
        let Event::Tile { px, w, .. } = next(&|e| matches!(e, Event::Tile { done: true, .. })) else { unreachable!() };
        let (a, b) = l.texel_to_phys();
        let v: Vec<f64> = (0..60 * 40).map(|i| (px.texel(i) * a + b) as f64).collect();
        assert!((v[7 * w as usize + 11] - 289.0625).abs() < 0.02, "{}", v[7 * 60 + 11]);
        assert!((v.iter().sum::<f64>() / v.len() as f64 - 284.9943).abs() < 0.02);
        e.probe(l.clone(), 11, 7);
        let Event::Probe { values, .. } = next(&|e| matches!(e, Event::Probe { .. })) else { unreachable!() };
        assert!((values[0].unwrap() - 289.0625).abs() < 1e-4, "{values:?}");
    }

    /// Layer math of two steps of a cube: the tile and the inspector are the function of the input values.
    #[test]
    fn math_of_two_layers() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/time_grid.nc");
        let (e, rx) = Engine::new(64 << 20, || {});
        let next = |f: &dyn Fn(&Event) -> bool| loop {
            let ev = rx.recv_timeout(Duration::from_secs(20)).expect("timeout");
            if f(&ev) {
                break ev;
            }
        };
        let opened = || match next(&|e| matches!(e, Event::Opened { .. })) {
            Event::Opened { res, .. } => res.unwrap(),
            _ => unreachable!(),
        };
        let probe = |l: &Arc<Layer>| {
            e.probe(l.clone(), 11, 7);
            let Event::Probe { values, .. } = next(&|e| matches!(e, Event::Probe { .. })) else { unreachable!() };
            values[l.band as usize].unwrap()
        };
        e.open(path.into());
        let a = opened();
        e.select(&a, a.var, a.choice, 2);
        let b = opened();
        // a - b where a > 285, else no data.
        e.math("d".into(), "K".into(), vec![a.clone(), b.clone()], Arc::new(|v| if v[0] > 285.0 { v[0] - v[1] } else { f64::NAN }), "test".into());
        let d = opened();
        let (va, vb) = (probe(&a), probe(&b));
        let want = if va > 285.0 { Some(va - vb) } else { None };
        e.probe(d.clone(), 11, 7);
        let Event::Probe { values, .. } = next(&|e| matches!(e, Event::Probe { .. })) else { unreachable!() };
        assert_eq!(values[0].map(|x| (x * 1e3).round()), want.map(|x| (x * 1e3).round()));
        e.want(1, vec![(d.clone(), TileKey { layer: d.id, lv: 0, tx: 0, ty: 0 })]);
        let Event::Tile { px, w, .. } = next(&|e| matches!(e, Event::Tile { done: true, .. })) else { unreachable!() };
        let (k, o) = d.texel_to_phys();
        let t = px.texel(7 * w as usize + 11) * k + o;
        match want {
            Some(x) => assert!((t as f64 - x).abs() < 0.05, "{t} {x}"),
            None => assert!(t.is_nan()),
        }
    }

    #[test]
    fn aggregates_skip_no_data() {
        let v = [3.0, f32::NAN, 1.0, 2.0, 10.0];
        let of = |a: Agg| a.of(&mut v.clone());
        assert_eq!((of(Agg::Mean), of(Agg::Median), of(Agg::Min), of(Agg::Max), of(Agg::Count)), (4.0, 2.5, 1.0, 10.0, 4.0));
        assert!((of(Agg::Std) - 3.5355).abs() < 1e-3);
        assert!(Agg::Mean.of(&mut [f32::NAN]).is_nan() && Agg::Count.of(&mut [f32::NAN]) == 0.0);
        // The tile sums give the same results.
        for a in [Agg::Mean, Agg::Std, Agg::Min, Agg::Max, Agg::Count, Agg::Median] {
            let mut acc = Acc::new(a, 1);
            v.iter().for_each(|&x| acc.add(vec![x]));
            assert!((acc.result(a)[0] - of(a)).abs() < 1e-5, "{a:?}");
        }
    }
}
