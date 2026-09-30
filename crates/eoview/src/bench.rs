//! Benchmark mode: `eoview --bench <file> [frames]`. It measures the targets of section 4 of the
//! specification on this machine and writes them to stdout. The window is 3840 x 2160 when the
//! screen permits it, without the vertical sync limit.
//!
//! To include the process start in "start to first window", set `EOVIEW_T0` to the start time in
//! nanoseconds since the Unix epoch (`EOVIEW_T0=$(date +%s%N) eoview --bench ...`).
use crate::View;
use eo_cache::{Engine, Layer};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use winit::event_loop::ActiveEventLoop;

const IDLE: Duration = Duration::from_secs(3);

pub enum Next {
    Redraw,
    Open(String),
    Select(usize),
    /// Draw again only at this time (or before, for other events).
    Wait(Instant),
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Start,
    Opening,
    Loading,
    Pan(usize),
    /// Band change to a new band (it loads data), to a second band, then back to the first band (resident tiles).
    BandNew,
    BandOther,
    BandBack(u32),
    /// Wait for the loads of the last test, then measure the idle CPU.
    Settle,
    Idle(Instant, f64),
}

pub struct Bench {
    t0: Instant,
    file: String,
    frames: usize,
    stage: Stage,
    t_stage: Instant,
    uploaded: bool,
    /// Set at each frame: true if the view has missing tiles.
    pub missing: bool,
    times: Vec<f64>,
    last: Option<Instant>,
    fit: (f64, [f64; 2]),
    idle_frames: u32,
    /// Layer of the first band of the band change test.
    first: u64,
}

fn cpu_seconds() -> f64 {
    // Fields 14 and 15 of /proc/self/stat (utime, stime), in clock ticks of 10 ms.
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let f: Vec<&str> = s.rsplit(')').next().unwrap_or("").split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (t(11) + t(12)) / 100.0
}

fn status(key: &str) -> String {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find(|l| l.starts_with(key)).map_or("?".into(), |l| l[key.len()..].trim().to_string())
}

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

fn report(k: &str, v: String) {
    println!("{k:<40} {v}");
}

impl Bench {
    pub fn new(t0: Instant, args: &[String]) -> Bench {
        let file = args.first().cloned().expect("usage: eoview --bench <file> [frames]");
        let frames = args.get(1).and_then(|f| f.parse().ok()).unwrap_or(600);
        let t = Instant::now();
        Bench {
            t0,
            file,
            frames,
            stage: Stage::Start,
            t_stage: t,
            uploaded: false,
            missing: true,
            times: Vec::with_capacity(frames),
            last: None,
            fit: (1.0, [0.0; 2]),
            idle_frames: 0,
            first: 0,
        }
    }

    pub fn uploaded(&mut self) {
        self.uploaded = true;
    }

    /// The layer is ready: header read, value sample made.
    pub fn opened(&mut self) {
        if self.stage == Stage::Opening {
            report("open to layer ready (header, sample)", ms(self.t_stage.elapsed()));
        }
    }

    /// Set the camera for the pan and zoom test: two zoom cycles to 32x, and three circles.
    pub fn drive(&mut self, v: &mut View) {
        if let Stage::Pan(i) = self.stage {
            let t = i as f64 / self.frames as f64;
            let tau = std::f64::consts::TAU;
            let z = 5.0 * (0.5 - 0.5 * (tau * 2.0 * t).cos());
            // Circles of a quarter of the fit view size.
            let (w, h) = (v.px.width() as f64 / self.fit.0, v.px.height() as f64 / self.fit.0);
            let r = 0.25 * (1.0 - 0.5 * (tau * 3.0 * t).cos());
            v.scale = self.fit.0 * 2f64.powf(z);
            v.center = [self.fit.1[0] + r * w * (tau * 3.0 * t).cos(), self.fit.1[1] + r * h * (tau * 3.0 * t).sin()];
        }
    }

    fn complete(&self, engine: &Engine) -> bool {
        !self.missing && engine.stats().running == 0
    }

    pub fn presented(&mut self, engine: &Engine, v: &View, el: &ActiveEventLoop) -> Next {
        let now = Instant::now();
        let dt = now - self.t_stage;
        let up = std::mem::take(&mut self.uploaded);
        match self.stage {
            Stage::Start => {
                report("start to first window (from main)", ms(now - self.t0));
                if let Some(t) = std::env::var("EOVIEW_T0").ok().and_then(|t| t.parse::<u128>().ok()) {
                    let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
                    report("start to first window (from exec)", format!("{:.1} ms", (n.saturating_sub(t)) as f64 / 1e6));
                }
                self.stage = Stage::Opening;
                self.t_stage = now;
                return Next::Open(self.file.clone());
            }
            Stage::Opening if up => {
                report("open to first pixels", ms(dt));
                self.stage = Stage::Loading;
            }
            Stage::Opening => {}
            Stage::Loading if self.complete(engine) => {
                report("open to complete view", ms(dt));
                if let Some(i) = v.inputs.first() {
                    let (w, h) = i.layer.size();
                    report("image", format!("{w} x {h}, {} levels, {} input(s), {}", i.layer.levels.len(), v.inputs.len(), i.layer.ds.product.desc));
                }
                report("view size", format!("{} x {} px", v.px.width(), v.px.height()));
                self.fit = (v.scale, v.center);
                self.stage = Stage::Pan(0);
                self.last = None;
            }
            Stage::Loading => {}
            Stage::Pan(i) => {
                if let Some(l) = self.last {
                    self.times.push((now - l).as_secs_f64() * 1000.0);
                }
                self.last = Some(now);
                if i + 1 < self.frames {
                    self.stage = Stage::Pan(i + 1);
                } else {
                    let t = &mut self.times;
                    t.sort_by(f64::total_cmp);
                    let mean = t.iter().sum::<f64>() / t.len() as f64;
                    report("pan and zoom frames", format!("{}", t.len()));
                    report("pan and zoom mean frame", format!("{mean:.2} ms ({:.0} fps)", 1000.0 / mean));
                    report("pan and zoom p99 frame", format!("{:.2} ms", t[t.len() * 99 / 100]));
                    report("pan and zoom max frame", format!("{:.2} ms", t[t.len() - 1]));
                    report("pan and zoom frames > 16 ms", format!("{}", t.iter().filter(|&&x| x > 16.0).count()));
                    self.t_stage = now;
                    let bands: usize = v.inputs.first().map_or(0, |i| i.layer.ds.product.vars.iter().map(|v| Layer::choices(v).len()).sum());
                    if bands > 2 {
                        self.stage = Stage::BandNew;
                        return Next::Select(1);
                    }
                    return self.idle();
                }
            }
            Stage::BandNew if v.inputs.len() == 1 && self.complete(engine) => {
                report("band change, new band, complete view", ms(dt));
                self.first = v.inputs[0].layer.id;
                self.stage = Stage::BandOther;
                return Next::Select(2);
            }
            Stage::BandNew => {}
            Stage::BandOther if v.inputs.first().is_some_and(|i| i.layer.id != self.first) && self.complete(engine) => {
                self.stage = Stage::BandBack(0);
                self.t_stage = now;
                return Next::Select(1);
            }
            Stage::BandOther => {}
            Stage::BandBack(n) => {
                let shown = v.inputs.first().is_some_and(|i| i.layer.id == self.first);
                if shown && !self.missing {
                    report("band change back, complete view", format!("{} frame(s), {}", n + 1, ms(dt)));
                    return self.idle();
                }
                self.stage = Stage::BandBack(n + 1);
            }
            Stage::Settle if self.complete(engine) => {
                let t = Instant::now();
                self.stage = Stage::Idle(t, cpu_seconds());
                return Next::Wait(t + IDLE);
            }
            Stage::Settle => {}
            Stage::Idle(t, cpu) => {
                self.idle_frames += 1;
                if now < t + IDLE {
                    return Next::Wait(t + IDLE);
                }
                let c = (cpu_seconds() - cpu) / (now - t).as_secs_f64() * 100.0;
                report("idle CPU", format!("{c:.1} % of one core, {} frame(s) in {IDLE:?}", self.idle_frames - 1));
                let s = engine.stats();
                report("RAM peak (VmHWM, with mapped file pages)", status("VmHWM:"));
                report("RAM now, heap (RssAnon)", status("RssAnon:"));
                report("RAM now, mapped files (RssFile)", status("RssFile:"));
                report("RAM budget, caches", format!("{} MB, raw {} MB, decoded {} MB", s.limit >> 20, s.raw >> 20, s.dec >> 20));
                el.exit();
                return Next::Wait(now);
            }
        }
        Next::Redraw
    }

    fn idle(&mut self) -> Next {
        self.stage = Stage::Settle;
        Next::Redraw
    }
}
