//! The animate workspace: the scene of an animation (one view), a preview at a lower quality, the
//! settings, the timeline and the render. It is the same idea as the workspace of a 3D program: make the
//! scene with a fast preview, then render the frames.
//!
//! The preview draws the scene with the code of the frames (`App::preview_frame`), at 1/2, 1/4, 1/8 or
//! 1/16 of their size. A smaller preview reads coarser levels of the data: it is fast with large data.

use crate::lang::{t, tf};
use crate::app::{App, Dialog};
use crate::render::{SIZES, frames};
use crate::ui::Cmd;
use egui::{Color32, Rect, Sense, Stroke, StrokeKind, vec2};

impl App {
    /// Open or close the animate workspace. The scene is the active view.
    pub fn set_animate(&mut self, on: bool) {
        if on == self.animate {
            return;
        }
        if on {
            // The camera of the scene: the area that the view shows now.
            let cam = self.pane(self.active).filter(|p| p.v.px.width() > 1.0 && p.v.scale > 0.0 && !p.layers.is_empty()).map(|p| (p.v.center, p.v.px.width() as f64 / p.v.scale));
            self.render_set.view = cam;
            // A new animation of a cube with a time axis longer than its data: the steps with data.
            let valid = self.pane(self.active).and_then(|p| p.timed()).and_then(|l| l.valid);
            if let (Some((a, b)), 0, None) = (valid, self.render_set.first, self.render_set.last) {
                (self.render_set.first, self.render_set.last) = (a, Some(b));
            }
            self.animate = true;
        } else {
            if let (Some(id), Some((center, width))) = (self.scene_pane(), self.render_set.view) {
                self.cam_restore = Some((id, center, width));
            }
            (self.animate, self.anim_play) = (false, None);
        }
    }

    /// The animate workspace: the layers and their settings at the left (the side panel), the settings of
    /// the animation at the right, the timeline at the bottom, and the viewport.
    pub fn animate_ui(&mut self, ui: &mut egui::Ui, scene: u32, cmds: &mut Vec<(Cmd, u32)>) {
        let ctx = ui.ctx().clone();
        egui::Panel::bottom("status").show(ui, |ui| self.status(ui));
        egui::Panel::left("side").resizable(true).default_size(330.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.side(ui, cmds));
        });
        egui::Panel::right("animation").resizable(true).default_size(320.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.animation_panel(ui, scene, cmds));
        });
        egui::Panel::bottom("anim time").show(ui, |ui| self.animation_time(ui, scene));
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.viewport(ui, scene));
        // The preview shows the changes of the data that comes in, and the particles move.
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }

    /// The viewport: the preview in the shape of the frames. Drag moves the camera, the mouse wheel changes
    /// its width, a double-click shows all the data.
    fn viewport(&mut self, ui: &mut egui::Ui, scene: u32) {
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, ui.visuals().extreme_bg_color);
        let (fw, fh) = (self.render_set.width.max(16) as f32, self.render_set.height.max(16) as f32);
        let room = area.shrink(16.0);
        let k = (room.width() / fw).min(room.height() / fh).max(0.01);
        let frame = Rect::from_center_size(room.center(), vec2(fw * k, fh * k));
        let resp = ui.allocate_rect(frame, Sense::click_and_drag());
        // The last frame of a render that runs, else the preview.
        let (tex, text) = match (self.job_texture(), &self.preview) {
            (Some(x), _) => (Some(x), t("Render").to_string()),
            (None, Some(p)) => (Some(p.tex), tf("Preview 1/{}: {} x {} pixels", &[&self.render_set.proxy.to_string(), &p.w.to_string(), &p.h.to_string()])),
            _ => (None, String::new()),
        };
        if let Some(t) = tex {
            ui.painter().image(t, frame, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
        ui.painter().rect_stroke(frame, 0.0, Stroke::new(1.0, Color32::from_gray(110)), StrokeKind::Outside);
        ui.painter().text(frame.left_bottom() + vec2(0.0, 4.0), egui::Align2::LEFT_TOP, text, egui::FontId::proportional(12.0), ui.visuals().weak_text_color());
        if self.job.is_some() {
            return;
        }
        // The camera: a center and a width in display units. One point of the viewport is `unit` of them.
        let Some((mut center, mut width)) = self.render_set.view else { return };
        let (globe, lat) = self.pane(scene).map_or((false, 0.0), |p| (p.v.globe, p.v.center[1]));
        let unit = width / frame.width().max(1.0) as f64;
        if resp.dragged() {
            let d = resp.drag_delta();
            // Globe: a degree of longitude is shorter away from the equator.
            let kx = if globe { lat.to_radians().cos().max(0.05) } else { 1.0 };
            center = [center[0] - d.x as f64 * unit / kx, center[1] + d.y as f64 * unit];
        }
        if resp.hovered() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            width /= pinch as f64 * 2f64.powf(scroll as f64 / 200.0);
        }
        if globe {
            center[1] = center[1].clamp(-89.0, 89.0);
        }
        self.render_set.view = Some((center, width.max(1e-9)));
        if resp.double_clicked() {
            self.fit_scene(scene);
        }
    }

    /// The camera shows all the data of the scene.
    fn fit_scene(&mut self, scene: u32) {
        self.render_set.view = None;
        if let Some(p) = self.pane_mut(scene) {
            (p.v.fit, p.fit_user) = (true, true);
        }
    }

    /// Timeline of the animation: play the preview, and the step of the preview in the steps of the render.
    fn animation_time(&mut self, ui: &mut egui::Ui, scene: u32) {
        let Some((n, step, label)) = self.pane(scene).and_then(|p| p.timed()).map(|l| (l.steps.len(), l.step, l.step_label(l.step))) else {
            ui.weak(t("The scene has no layer with time steps: the animation has one frame."));
            return;
        };
        let (first, stride) = (self.render_set.first.min(n - 1), self.render_set.stride.max(1));
        let last = self.render_set.last.unwrap_or(n - 1).clamp(first, n - 1);
        let now = ui.input(|i| i.time);
        let mut go = None;
        ui.horizontal(|ui| {
            if ui.selectable_label(self.anim_play.is_some(), if self.anim_play.is_some() { t("Pause") } else { t("Play") }).on_hover_text(t("Play the time steps of the animation in the preview")).clicked() {
                self.anim_play = if self.anim_play.is_some() { None } else { Some(now) };
            }
            ui.monospace(format!("{label}   {}", tf("step {} of {}", &[&(step + 1).to_string(), &n.to_string()])));
            let mut cur = step.clamp(first, last);
            ui.spacing_mut().slider_width = (ui.available_width() - 12.0).max(60.0);
            if ui.add(egui::Slider::new(&mut cur, first..=last).show_value(false)).changed() {
                go = Some(first + (cur - first) / stride * stride);
            }
        });
        // The preview plays at the rate of the animation, one step of the interval for each frame.
        if let Some(next) = self.anim_play
            && now >= next
        {
            let s = if step + stride > last || step < first { first } else { step + stride };
            go = Some(s);
            self.anim_play = Some((next + 1.0 / self.render_set.fps.max(0.2) as f64).max(now));
        }
        // The preview shows a step of the animation. A data cube opens at its first step, which can be
        // years before the steps of the animation, and without data.
        if go.is_none() && (step < first || step > last) && self.job.is_none() {
            go = Some(first);
        }
        if let Some(s) = go.filter(|&s| s != step) {
            self.set_time(scene, s);
        }
    }

    /// Settings of the animation: preview, camera, overlays, time, output, and the render.
    fn animation_panel(&mut self, ui: &mut egui::Ui, scene: u32, cmds: &mut Vec<(Cmd, u32)>) {
        let busy = self.job.is_some();
        let n = self.pane(scene).and_then(|p| p.timed()).map_or(1, |l| l.steps.len());
        let label = |s: usize| self.pane(scene).and_then(|p| p.timed()).map_or(String::new(), |l| l.step_label(s));
        let (first, last) = (self.render_set.first.min(n - 1), self.render_set.last.unwrap_or(n - 1).min(n - 1));
        let (first_label, last_label) = (label(first), label(last));
        let (globe, mut overlays, empty) = self.pane(scene).map_or((false, Default::default(), true), |p| (p.v.globe, p.overlays, p.layers.is_empty()));
        let mut smooth = self.pane(scene).is_some_and(|p| p.smooth);
        // A date that the user typed for the first step (0) or the last step (1).
        let mut typed: Option<(usize, f64)> = None;
        if self.ffmpeg_found.is_none() {
            self.ffmpeg_found = Some(crate::render::ffmpeg(&self.prefs.ffmpeg).is_some());
        }
        let no_ffmpeg = self.ffmpeg_found == Some(false) && crate::render::VIDEO_EXT.iter().any(|e| self.render_set.out.to_lowercase().ends_with(&format!(".{e}")));
        let (mut fit, mut browse) = (false, false);
        let section = |ui: &mut egui::Ui, name: &str| {
            ui.add_space(6.0);
            ui.strong(name);
        };
        ui.add_enabled_ui(!busy, |ui| {
            let (set, dates) = (&mut self.render_set, &mut self.anim_dates);
            section(ui, t("Preview"));
            ui.horizontal(|ui| {
                ui.label(t("Quality"));
                for k in [2, 4, 8, 16] {
                    ui.selectable_value(&mut set.proxy, k, format!("1/{k}")).on_hover_text(t("Size of the preview, as a part of the size of the frames. A smaller preview is faster and reads less data"));
                }
            });

            section(ui, t("Camera"));
            ui.horizontal(|ui| {
                if ui.selectable_label(!globe, t("Map")).clicked() == ui.selectable_label(globe, t("Globe")).clicked() {
                } else {
                    cmds.push((Cmd::Globe, scene));
                    set.view = None;
                }
                fit = ui.button(t("Fit")).on_hover_text(t("Show all the data (also: a double-click in the viewport)")).clicked();
            });
            if let Some((c, w)) = &mut set.view {
                egui::Grid::new("camera").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label(t("Center"));
                    let speed = *w / 400.0;
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut c[0]).speed(speed).max_decimals(4));
                        ui.add(egui::DragValue::new(&mut c[1]).speed(speed).max_decimals(4));
                    });
                    ui.end_row();
                    ui.label(t("Width"));
                    let speed = *w / 200.0;
                    ui.add(egui::DragValue::new(w).speed(speed).range(1e-9..=1e9).max_decimals(4)).on_hover_text(t("Width of the frame in the units of the display CRS (degrees for longitude and latitude)"));
                    ui.end_row();
                });
            }

            section(ui, t("Overlays"));
            ui.checkbox(&mut overlays.coasts, t("Coasts"));
            ui.checkbox(&mut overlays.borders, t("Country borders"));
            ui.checkbox(&mut overlays.names, t("Country names"));
            ui.checkbox(&mut set.stamp, t("Time and legend"));
            ui.horizontal(|ui| {
                ui.label(t("Legend"));
                ui.add(egui::TextEdit::singleline(&mut set.legend).hint_text(t("The name of the layer and its unit")).desired_width(190.0));
            });
            ui.checkbox(&mut smooth, t("Smooth pixels")).on_hover_text(t("Linear between the pixels of the data, not squares: for data at a low resolution"));

            section(ui, t("Time"));
            egui::Grid::new("time").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                // The interface counts the steps from 1.
                // The step as a number, or as a date: the step nearest to the date that the user types.
                let mut date = |ui: &mut egui::Ui, k: usize, label: &str| {
                    let mut text = if dates[k].is_empty() { label.to_string() } else { dates[k].clone() };
                    let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(140.0)).on_hover_text(t("A date and a time, for example 2025-01-01 12:00:00. Enter: the step nearest to it"));
                    if r.lost_focus() {
                        typed = eo_core::time::parse(text.trim()).map(|t| (k, t)).or(typed);
                    }
                    dates[k] = if r.has_focus() { text } else { String::new() };
                };
                ui.label(t("First step"));
                ui.horizontal(|ui| {
                    let mut v = first + 1;
                    if ui.add(egui::DragValue::new(&mut v).range(1..=n)).changed() {
                        set.first = v - 1;
                    }
                    date(ui, 0, &first_label);
                });
                ui.end_row();
                ui.label(t("Last step"));
                ui.horizontal(|ui| {
                    let mut v = last + 1;
                    if ui.add(egui::DragValue::new(&mut v).range(1..=n)).changed() {
                        set.last = Some(v - 1).filter(|l| l + 1 < n);
                    }
                    date(ui, 1, &last_label);
                });
                ui.end_row();
                ui.label(t("Interval"));
                ui.add(egui::DragValue::new(&mut set.stride).range(1..=n.max(1)).suffix(t(" step(s)")));
                ui.end_row();
                ui.label(t("Frames for a step"));
                ui.add(egui::DragValue::new(&mut set.sub).range(1..=120)).on_hover_text(t("More than 1: the frames between two steps are a blend of the two steps, for a smooth change"));
                ui.end_row();
            });

            section(ui, t("Output"));
            egui::Grid::new("output").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label(t("Size"));
                let cur = SIZES.iter().find(|x| (x.1, x.2) == (set.width, set.height)).map_or(t("Custom"), |x| x.0);
                egui::ComboBox::from_id_salt("anim size").selected_text(cur).show_ui(ui, |ui| {
                    for (name, w, h) in SIZES {
                        if ui.selectable_label((set.width, set.height) == (*w, *h), *name).clicked() {
                            (set.width, set.height) = (*w, *h);
                        }
                    }
                });
                ui.end_row();
                ui.label("");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut set.width).range(16..=16384));
                    ui.label("x");
                    ui.add(egui::DragValue::new(&mut set.height).range(16..=16384));
                });
                ui.end_row();
                ui.label(t("Rate"));
                ui.add(egui::DragValue::new(&mut set.fps).range(1.0..=120.0).speed(0.2).suffix(t(" frames/s")));
                ui.end_row();
                ui.label(t("File"));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut set.out).desired_width(150.0)).on_hover_text(t("A video file (mp4, mov, mkv, webm, gif), or a directory for PNG files"));
                    browse = ui.button(t("Browse...")).clicked();
                });
                ui.end_row();
            });
            ui.checkbox(&mut set.keep, t("Keep the frames")).on_hover_text(t("The frames are PNG files next to the video, and the video comes at the end. A render that stopped continues after its last frame: for a long render"));
            let total = frames(set.steps(n).len(), set.sub);
            ui.add_space(4.0);
            ui.weak(tf("{} frames, {} s of video. The render reads the data of one frame at a time.", &[&total.to_string(), &format!("{:.1}", total as f32 / set.fps.max(0.01))]));
        });
        if no_ffmpeg {
            ui.colored_label(ui.visuals().warn_fg_color, t("ffmpeg was not found: the frames will be PNG files. Set the path of ffmpeg in the preferences."));
        }
        ui.separator();
        match self.job.as_ref().map(|j| (j.done, j.frames(), j.t0.elapsed().as_secs_f32(), j.pane)) {
            Some((done, total, secs, pane)) => {
                let rate = done as f32 / secs.max(1e-3);
                let wait = self.frame_wait(pane).filter(|_| done == 0 || rate < 0.5).map_or(String::new(), |w| tf("Waits for {}", &[&w]));
                let left = if rate > 0.0 { tf("{} s left", &[&format!("{:.0}", (total - done) as f32 / rate)]) } else { String::new() };
                ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).text(format!("{}   {rate:.1} {}   {left}", tf("Frame {} of {}", &[&done.to_string(), &total.to_string()]), t("frames/s"))));
                ui.weak(wait);
                if ui.button(t("Stop")).clicked() {
                    self.cancel_render();
                }
            }
            None => {
                if ui.add_enabled(!empty, egui::Button::new(t("Render animation"))).clicked() {
                    self.anim_play = None;
                    self.render_start(scene);
                }
                if let Some(m) = &self.render_msg {
                    ui.label(m);
                }
            }
        }
        if let Some(p) = self.pane_mut(scene) {
            (p.overlays, p.smooth) = (overlays, smooth);
        }
        if let Some((k, s)) = typed.and_then(|(k, t)| Some((k, self.step_at(scene, t)?))) {
            if k == 0 {
                self.render_set.first = s;
            } else {
                self.render_set.last = Some(s).filter(|l| l + 1 < n);
            }
        }
        if fit {
            self.fit_scene(scene);
        }
        if browse {
            self.dialog = Some(Dialog::RenderOut);
        }
    }
}
