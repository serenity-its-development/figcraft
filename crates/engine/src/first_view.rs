//! Where the host's read-only first view was left, as a place on the canvas (OneOS progressive open, 2026-10-27).
//!
//! The first view showed the design as a scrolling picture; the host says how far across and down it was (fractions of the scrollable
//! extent) and which text, if any, was selected. The editor opens at the same PLACE ON THE PAGE: the viewport's top-left corner sits at
//! that fraction of the page's content (the same frames are in view), and the text layer holding the selected text (the one nearest the
//! top edge) is the selection, with the top-level frame it sits in reported as the frame in view. Pure: the document in, a place out;
//! [`Session::open_at_first_view`] puts the active document there (a selection and a view, never an edit).

use figcraft_doc::{Document, NodeId};
use figcraft_geom::Rect;

use crate::{Result, Session};

/// The first view as a canvas place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FirstViewPlace {
    /// Page coordinate for the viewport centre.
    pub cx: f64,
    pub cy: f64,
    /// The text layer to select.
    pub node: Option<NodeId>,
    /// The top-level frame the top edge is in (or holds the selected text).
    pub frame: Option<NodeId>,
}

fn fraction(v: f64) -> f64 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

/// Where the viewport's near edge goes: `f` of the way through what can scroll (content size minus viewport size, never negative).
fn edge(lo: f64, content: f64, shown: f64, f: f64) -> f64 {
    lo + f * (content - shown).max(0.0)
}

/// Every text layer under `id` (depth first, bounded), with the text it holds.
fn texts(doc: &Document, id: NodeId, out: &mut Vec<(NodeId, String)>, depth: usize) {
    if depth > 64 || out.len() > 20_000 {
        return;
    }
    let Some(n) = doc.node(id) else { return };
    if let Some(t) = n.kind.text() {
        out.push((id, t.characters.clone()));
    }
    for c in &n.children {
        texts(doc, *c, out, depth + 1);
    }
}

/// The place for a first view scrolled to `(x_fraction, y_fraction)` of a viewport `view_w` x `view_h` page units, with `selection`
/// selected. `None` for an empty page.
pub fn locate(doc: &Document, page: NodeId, selection: &str, x_fraction: f64, y_fraction: f64, view_w: f64, view_h: f64) -> Option<FirstViewPlace> {
    let bounds: Rect = doc.page_content_bounds(page)?;
    let (fx, fy) = (fraction(x_fraction), fraction(y_fraction));
    let (vw, vh) = (view_w.max(1.0), view_h.max(1.0));
    let left = edge(bounds.x0, bounds.width(), vw, fx);
    let top = edge(bounds.y0, bounds.height(), vh, fy);
    // A page smaller than the viewport sits in the middle of it.
    let cx = if bounds.width() <= vw { bounds.center().x } else { left + vw / 2.0 };
    let cy = if bounds.height() <= vh { bounds.center().y } else { top + vh / 2.0 };
    let frames = &doc.node(page)?.children;
    let frame_at = |y: f64| {
        frames
            .iter()
            .filter_map(|f| doc.world_bounds(*f).map(|b| (*f, b)))
            .min_by(|(_, a), (_, b)| {
                let d = |r: &Rect| {
                    if y < r.y0 {
                        r.y0 - y
                    } else if y > r.y1 {
                        y - r.y1
                    } else {
                        0.0
                    }
                };
                d(a).total_cmp(&d(b))
            })
            .map(|(f, _)| f)
    };
    let wanted = selection.lines().map(str::trim).find(|l| !l.is_empty());
    if let Some(wanted) = wanted {
        let mut all = Vec::new();
        for f in frames {
            texts(doc, *f, &mut all, 0);
        }
        let near = |id: NodeId| {
            doc.world_bounds(id).map_or(f64::MAX, |b| {
                if top < b.y0 {
                    b.y0 - top
                } else if top > b.y1 {
                    top - b.y1
                } else {
                    0.0
                }
            })
        };
        let best =
            all.iter().filter(|(_, t)| t.trim() == wanted || t.lines().any(|l| l.trim() == wanted)).min_by(|a, b| near(a.0).total_cmp(&near(b.0)));
        if let Some((id, _)) = best {
            let frame = frames.iter().copied().find(|f| *f == *id || is_under(doc, *f, *id)).or_else(|| frame_at(top));
            return Some(FirstViewPlace { cx, cy, node: Some(*id), frame });
        }
    }
    Some(FirstViewPlace { cx, cy, node: None, frame: frame_at(top) })
}

fn is_under(doc: &Document, ancestor: NodeId, id: NodeId) -> bool {
    fn walk(doc: &Document, n: NodeId, id: NodeId, depth: usize) -> bool {
        if depth > 64 {
            return false;
        }
        doc.node(n).is_some_and(|node| node.children.iter().any(|c| *c == id || walk(doc, *c, id, depth + 1)))
    }
    walk(doc, ancestor, id, 0)
}

impl Session {
    /// Open the active document at the first view's place: the view moves to it and the text layer holding the selected text is the
    /// selection. Not an edit (nothing is undoable). False when the page has nothing to show.
    pub fn open_at_first_view(&mut self, selection: &str, x_fraction: f64, y_fraction: f64) -> Result<bool> {
        let ds = self.ds()?;
        let zoom = ds.view.zoom.max(1e-6);
        let (vw, vh) = (ds.view.vw / zoom, ds.view.vh / zoom);
        let Some(place) = locate(&ds.doc, ds.page, selection, x_fraction, y_fraction, vw, vh) else { return Ok(false) };
        let ds = self.ds_mut()?;
        ds.view.cx = place.cx;
        ds.view.cy = place.cy;
        ds.selection = place.node.into_iter().collect();
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params;
    use serde_json::{Value, json};

    fn id(v: &Value) -> NodeId {
        params::id_of(&v["id"]).unwrap()
    }

    /// Three frames one above the other (0, 1000, 2000 high 800), each holding a text layer; the third holds "Pricing" too.
    fn design() -> (Session, Vec<NodeId>) {
        let mut s = Session::new();
        let mut frames = Vec::new();
        for i in 0..3 {
            let f = id(&s.execute("frame.create", &json!({"x": 0, "y": i * 1000, "width": 600, "height": 800})).unwrap());
            frames.push(f);
            s.execute("text.create", &json!({"text": format!("Heading {}", i + 1), "x": 20, "y": i * 1000 + 20, "parent": f})).unwrap();
        }
        s.execute("text.create", &json!({"text": "Pricing", "x": 20, "y": 2100, "parent": frames[2]})).unwrap();
        (s, frames)
    }

    fn place(s: &Session, sel: &str, fy: f64) -> FirstViewPlace {
        let ds = s.ds().unwrap();
        locate(&ds.doc, ds.page, sel, 0.0, fy, 600.0, 500.0).unwrap()
    }

    #[test]
    fn the_viewport_top_is_the_fraction_of_the_scrollable_height() {
        let (s, frames) = design();
        // Content is 0..2800 high, the viewport 500: the top edge runs 0..2300.
        assert_eq!(place(&s, "", 0.0).cy, 250.0);
        assert_eq!(place(&s, "", 1.0).cy, 2300.0 + 250.0);
        assert_eq!(place(&s, "", 0.5).cy, 1150.0 + 250.0);
        assert_eq!(place(&s, "", 0.0).frame, Some(frames[0]));
        assert_eq!(place(&s, "", 0.5).frame, Some(frames[1]));
        assert_eq!(place(&s, "", f64::NAN).cy, 250.0, "hostile numbers are the top");
    }

    #[test]
    fn the_selected_text_is_the_layer_nearest_the_top_edge_and_its_frame_is_in_view() {
        let (s, frames) = design();
        let p = place(&s, "Heading 2", 0.9);
        assert!(p.node.is_some());
        assert_eq!(p.frame, Some(frames[1]), "found wherever the view was");
        let pricing = place(&s, "Pricing", 0.0);
        assert_eq!(pricing.frame, Some(frames[2]));
        assert_eq!(place(&s, "not on this page", 0.0).node, None);
    }

    #[test]
    fn a_page_smaller_than_the_viewport_sits_in_its_middle() {
        let mut s = Session::new();
        s.execute("frame.create", &json!({"x": 100, "y": 100, "width": 200, "height": 100})).unwrap();
        let ds = s.ds().unwrap();
        let p = locate(&ds.doc, ds.page, "", 0.7, 0.7, 600.0, 500.0).unwrap();
        assert_eq!((p.cx, p.cy), (200.0, 150.0));
    }

    #[test]
    fn opening_at_the_place_moves_the_view_and_selects_the_layer_without_an_edit() {
        let (mut s, _) = design();
        let doc_before = s.ds().unwrap().doc.clone();
        let undo = s.ds().unwrap().history.undo.len();
        assert!(s.open_at_first_view("Heading 3", 0.0, 1.0).unwrap());
        let ds = s.ds().unwrap();
        assert_eq!(ds.selection.len(), 1);
        assert_eq!(ds.doc.node(ds.selection[0]).and_then(|n| n.kind.text()).map(|t| t.characters.as_str()), Some("Heading 3"));
        assert!(ds.view.cy > 1000.0, "the view is down the page: {}", ds.view.cy);
        assert_eq!(ds.doc, doc_before, "not an edit");
        assert_eq!(ds.history.undo.len(), undo);
        let mut empty = Session::new();
        assert!(!empty.open_at_first_view("", 0.0, 0.0).unwrap(), "an empty page has no place");
    }
}
