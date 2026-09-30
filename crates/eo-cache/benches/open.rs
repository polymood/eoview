//! Benchmark: open to first pixels (specification section 4), without the GPU and the window.
//! The time goes from the open request to the first pixels (complete or partial tile) of the coarsest
//! display level.
//! The file is in the page cache after the first iteration (warm cache).
//!
//! Files: testdata/cog_u16.tif, and the files in EOVIEW_BENCH_FILES (separated with ':'), for
//! example the files of scripts/make_bench_data.sh.
//! For the GUI targets (start time, frame times, idle CPU), run `eoview --bench <file>`.
use criterion::{Criterion, criterion_group, criterion_main};
use eo_cache::{Engine, Event, TileKey};
use std::time::{Duration, Instant};

fn open_to_first_pixels(e: &Engine, rx: &std::sync::mpsc::Receiver<Event>, path: &str) -> Duration {
    let t = Instant::now();
    e.open(path.into());
    let l = loop {
        if let Event::Opened { res, .. } = rx.recv().unwrap() {
            break res.unwrap();
        }
    };
    let top = l.levels.len() - 1;
    let key = TileKey { layer: l.id, lv: top as u8, tx: 0, ty: 0 };
    e.want(0, vec![(l.clone(), key)]);
    loop {
        if let Event::Tile { key: k, .. } = rx.recv().unwrap()
            && k == key
        {
            break;
        }
    }
    let d = t.elapsed();
    e.want(0, vec![]);
    d
}

fn bench(c: &mut Criterion) {
    let mut files = vec![concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/cog_u16.tif").to_string()];
    if let Ok(v) = std::env::var("EOVIEW_BENCH_FILES") {
        files.extend(v.split(':').filter(|s| !s.is_empty()).map(String::from));
    }
    for f in files {
        let name: Vec<&str> = f.rsplit('/').take(2).collect();
        c.bench_function(&format!("open to first pixels: {}/{}", name[1], name[0]), |b| {
            b.iter_custom(|n| {
                // A new engine for each open: no data in the caches.
                (0..n)
                    .map(|_| {
                        let (e, rx) = Engine::new(1 << 30, || {});
                        open_to_first_pixels(&e, &rx, &f)
                    })
                    .sum()
            })
        });
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10).measurement_time(Duration::from_secs(5));
    targets = bench
}
criterion_main!(benches);
