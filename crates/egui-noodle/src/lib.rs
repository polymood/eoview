//! A node graph editor for [egui](https://github.com/emilk/egui).
//!
//! - A true 2D canvas: the nodes, their fields, the links and the text are drawn with the painter of egui from
//!   graph coordinates. They stay sharp and they do not move against each other at any zoom.
//! - Fields ([`NodeUi`]): numbers that you drag, sliders, check boxes, lists, text and
//!   code. An egui text edit shows only while the user types.
//! - Links that you drag from a socket, a search menu to add a node (Shift+A), mute (M), collapse (H),
//!   duplicate (Shift+D), selection with a rectangle.
//! - Themes ([`Theme`]): colors, grid, links, sockets, headers and sizes. Four presets: `Theme::studio`,
//!   `light`, `blueprint`, `minimal`.
//! - Generic: the application keeps its data in [`Graph<N>`] and describes its nodes with a [`Viewer`].
//!   [`Response::changed`] tells a live application to compute its outputs again.
//!
//! ```no_run
//! use egui_noodle::{Editor, Graph, NodeUi, Viewer};
//!
//! #[derive(Clone)]
//! enum Op { Number(f64), Add }
//!
//! struct Ops;
//! impl Viewer<Op> for Ops {
//!     fn title(&mut self, n: &Op) -> String { match n { Op::Number(_) => "Number".into(), Op::Add => "Add".into() } }
//!     fn inputs(&mut self, n: &Op) -> Vec<String> { match n { Op::Number(_) => vec![], Op::Add => vec!["a".into(), "b".into()] } }
//!     fn body(&mut self, _id: u32, n: &mut Op, ui: &mut NodeUi) {
//!         if let Op::Number(v) = n { ui.value("value", v, 0.01); }
//!     }
//!     fn catalog(&mut self) -> Vec<(String, String, Op)> {
//!         vec![("Input".into(), "Number".into(), Op::Number(0.0)), ("Math".into(), "Add".into(), Op::Add)]
//!     }
//! }
//!
//! fn ui(ui: &mut egui::Ui, editor: &mut Editor, graph: &mut Graph<Op>) {
//!     if editor.show(ui, graph, &mut Ops).changed {
//!         // Compute the outputs again.
//!     }
//! }
//! ```

mod camera;
mod editor;
mod graph;
mod ui;

pub use camera::Camera;
pub use editor::{Editor, Labels, Response, Viewer};
pub use graph::{Graph, Node};
pub use ui::{Grid, Header, NodeUi, Socket, Theme, Wire};
