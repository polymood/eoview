//! Map overlays of a view: coasts, country borders and country names (Natural Earth, public domain).
//! `scripts/make_outlines.py` writes the data file. The points are longitude and latitude in degrees.

use crate::app::Pane;
use egui::{Align2, Color32, FontId, Pos2, Stroke};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// The overlays that a view shows.
#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overlays {
    pub coasts: bool,
    pub borders: bool,
    pub names: bool,
}

impl Overlays {
    pub fn any(&self) -> bool {
        self.coasts || self.borders || self.names
    }
}

pub struct Outlines {
    /// Kind (0 coast, 1 border), points, and limits (west, south, east, north).
    pub lines: Vec<(u8, Vec<[f64; 2]>, [f64; 4])>,
    /// Position, rank (0 is the most important) and name, the most important first.
    pub labels: Vec<([f64; 2], u8, String)>,
}

static DATA: LazyLock<Outlines> = LazyLock::new(|| parse(include_bytes!("../assets/outlines.bin")).unwrap_or(Outlines { lines: vec![], labels: vec![] }));

pub fn data() -> &'static Outlines {
    &DATA
}

fn parse(b: &[u8]) -> Option<Outlines> {
    let mut at = 5;
    let mut take = |n: usize| {
        let s = b.get(at..at + n);
        at += n;
        s
    };
    let u32le = |s: &[u8]| u32::from_le_bytes(s.try_into().unwrap()) as usize;
    let f32le = |s: &[u8]| f32::from_le_bytes(s.try_into().unwrap()) as f64;
    if b.get(..5)? != b"EOOL1" {
        return None;
    }
    let mut lines = vec![];
    for _ in 0..u32le(take(4)?) {
        let (kind, n) = (take(1)?[0], u32le(take(4)?));
        let pts: Vec<[f64; 2]> = take(n * 8)?.chunks_exact(8).map(|c| [f32le(&c[..4]), f32le(&c[4..])]).collect();
        let fold = |k: usize, f: fn(f64, f64) -> f64, v: f64| pts.iter().fold(v, |a, p| f(a, p[k]));
        let bbox = [fold(0, f64::min, f64::MAX), fold(1, f64::min, f64::MAX), fold(0, f64::max, f64::MIN), fold(1, f64::max, f64::MIN)];
        lines.push((kind, pts, bbox));
    }
    let mut labels = vec![];
    for _ in 0..u32le(take(4)?) {
        let h = take(10)?;
        let name = String::from_utf8_lossy(take(h[9] as usize)?).into_owned();
        labels.push(([f32le(&h[..4]), f32le(&h[4..8])], h[8], name));
    }
    labels.sort_by_key(|l| l.1);
    Some(Outlines { lines, labels })
}

/// Draw the overlays of the view. `pts`: the points of each line in the display CRS of the view, for a
/// view that is not in longitude and latitude (None: the display coordinates are longitude and latitude).
/// `label`: the display position of a longitude and latitude.
pub fn draw(p: &Pane, pt: &egui::Painter, ppp: f32, pts: Option<&[Vec<[f64; 2]>]>, label: &dyn Fn([f64; 2]) -> Option<[f64; 2]>) {
    let (o, rect, d) = (p.overlays, p.rect, data());
    if !o.any() {
        return;
    }
    let pt = pt.with_clip_rect(rect);
    let view = p.v.rect();
    // A flat view in longitude and latitude shows one world (as the data, `View::draws`).
    let flat = pts.is_none() && !p.v.globe;
    let shifts: &[f64] = &[0.0];
    let big = rect.expand(64.0);
    let w = 1.0 / ppp.min(1.0);
    for (i, (kind, ll, bbox)) in d.lines.iter().enumerate() {
        if (*kind == 0 && !o.coasts) || (*kind == 1 && !o.borders) {
            continue;
        }
        let line = pts.map_or(&ll[..], |v| &v[i][..]);
        for &s in shifts {
            if flat && (bbox[2] + s < view[0] || bbox[0] + s > view[2] || bbox[3] < view[1] || bbox[1] > view[3]) {
                continue;
            }
            // Parts of the line on the screen. A point on the far side of the globe, or far from the screen,
            // ends a part.
            let mut run: Vec<Pos2> = vec![];
            let flush = |run: &mut Vec<Pos2>| {
                if run.len() >= 2 {
                    let halo = Color32::from_black_alpha(if *kind == 0 { 150 } else { 110 });
                    let color = if *kind == 0 { Color32::from_white_alpha(235) } else { Color32::from_white_alpha(170) };
                    pt.add(egui::Shape::line(run.clone(), Stroke::new(2.6 * w, halo)));
                    pt.add(egui::Shape::line(std::mem::take(run), Stroke::new(if *kind == 0 { 1.1 } else { 0.9 } * w, color)));
                }
                run.clear();
            };
            for q in line {
                let sp = p.v.to_screen([q[0] + s, q[1]], rect);
                if q[0].is_finite() && big.contains(sp) {
                    run.push(sp);
                } else {
                    flush(&mut run);
                }
            }
            flush(&mut run);
        }
    }
    if o.names {
        // The most important names first. A name does not show on an other name.
        let mut used: Vec<egui::Rect> = vec![];
        for (ll, _, name) in &d.labels {
            let Some(q) = label(*ll) else { continue };
            let sp = p.v.to_screen(q, rect);
            if !rect.shrink(24.0).contains(sp) {
                continue;
            }
            let font = FontId::proportional(13.0);
            let g = pt.layout_no_wrap(name.clone(), font.clone(), Color32::WHITE);
            let r = Align2::CENTER_CENTER.anchor_size(sp, g.size()).expand(6.0);
            if used.iter().any(|u| u.intersects(r)) {
                continue;
            }
            used.push(r);
            for d in [[-1.0, 0.0], [1.0, 0.0], [0.0, -1.0], [0.0, 1.0]] {
                pt.text(sp + egui::vec2(d[0], d[1]), Align2::CENTER_CENTER, name, font.clone(), Color32::from_black_alpha(200));
            }
            pt.text(sp, Align2::CENTER_CENTER, name, font, Color32::WHITE);
        }
    }
}
