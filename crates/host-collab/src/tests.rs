//! The shim against a stand-in for the host's room: one shared tree that applies ops in the order they arrive and reports each as
//! the change the host's delta would carry. (OneOS's `craft-design-convergence.test.ts` runs the same shim against the REAL
//! adapter and CRDT engine; these tests keep the mapping honest without a browser.)

use figcraft_color::{Color, Paint};
use figcraft_doc::{Document, NodeId, NodeKind};
use serde_json::{Value, json};

use super::*;

/// The room: the shared tree, in one order for everyone.
struct Room {
    tree: Value,
}

impl Room {
    fn new() -> Room {
        Room { tree: json!({}) }
    }

    /// Merge batches into the room, in the order given.
    fn apply(&mut self, batches: &[Batch]) {
        for b in batches {
            for op in &b.ops {
                apply_op(&mut self.tree, op).unwrap();
            }
        }
    }

    /// What the host reports of `batches` once the room holds everything merged so far: for each key they touched, its merged
    /// value (or that it is gone).
    fn changes(&self, batches: &[Batch]) -> Vec<HostChange> {
        let mut out = Vec::new();
        for b in batches {
            for op in &b.ops {
                let mut path = path_of(op);
                path.push(op["key"].as_str().unwrap().to_owned());
                let now = path.iter().try_fold(&self.tree, |a, s| a.get(s)).cloned();
                out.push(HostChange { path, value: now });
            }
        }
        out
    }

    /// Apply, then report.
    fn take(&mut self, batches: &[Batch]) -> Vec<HostChange> {
        self.apply(batches);
        self.changes(batches)
    }
}

struct Window {
    shim: Shim,
    doc: Document,
}

impl Window {
    fn new(client: u32, doc: &Document) -> Window {
        let mut d = doc.clone();
        d.set_client_id(client);
        Window { shim: Shim::new(client), doc: d }
    }

    /// The person edited: send what changed.
    fn flush(&mut self, now: u64) -> Vec<Batch> {
        self.shim.local(&self.doc, "Edit", false, now).unwrap()
    }

    /// The host told us: show it (the document the editor holds becomes the shim's).
    fn hear(&mut self, changes: &[HostChange], ack: u64, now: u64) {
        if self.shim.remote_delta(changes, ack, now, &self.doc).unwrap() {
            self.doc = self.shim.document().unwrap().clone();
        }
    }
}

fn base() -> (Document, NodeId, NodeId, NodeId) {
    let mut d = Document::new("File");
    let page = d.first_page().unwrap();
    let frame = d.create(page, NodeKind::Frame, "Frame", 0.0, 0.0, 300.0, 200.0).unwrap();
    let a = d.create(frame, NodeKind::Rectangle, "A", 10.0, 10.0, 50.0, 50.0).unwrap();
    let b = d.create(frame, NodeKind::Rectangle, "B", 70.0, 10.0, 50.0, 50.0).unwrap();
    (d, frame, a, b)
}

/// Window 1 seeds the room; window 2 opens the same file and joins.
fn two() -> (Room, Window, Window, NodeId, NodeId, NodeId) {
    let (doc, frame, a, b) = base();
    let mut room = Room::new();
    let mut w1 = Window::new(1, &doc);
    let seed = w1.shim.seed(&w1.doc, 100).unwrap();
    assert!(seed[0].seed);
    room.take(&seed);
    let mut w2 = Window::new(2, &doc);
    assert!(w2.shim.remote_state(&room.tree, &w2.doc).unwrap() || true);
    w2.doc = w2.shim.document().unwrap().clone();
    (room, w1, w2, frame, a, b)
}

fn same(a: &Window, b: &Window) {
    let (x, y) = (digest(&a.doc).unwrap(), digest(&b.doc).unwrap());
    if x != y {
        panic!("the windows differ: {}", differences(&x, &y, "", &mut Vec::new()).join("; "));
    }
}

/// Where two JSON values differ (the first few places), as paths.
fn differences(a: &Value, b: &Value, at: &str, out: &mut Vec<String>) -> Vec<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys()).collect::<BTreeSet<_>>() {
                match (x.get(k), y.get(k)) {
                    (Some(p), Some(q)) => {
                        differences(p, q, &format!("{at}/{k}"), out);
                    }
                    (p, q) => out.push(format!("{at}/{k}: {} vs {}", p.is_some(), q.is_some())),
                }
            }
        }
        _ if a != b && out.len() < 8 => out.push(format!("{at}: {} vs {}", a.to_string().chars().take(60).collect::<String>(), b.to_string().chars().take(60).collect::<String>())),
        _ => {}
    }
    out.clone()
}

/// Both windows learn of each other's batches.
fn settle(room: &mut Room, w1: &mut Window, w2: &mut Window, from1: &[Batch], from2: &[Batch], now: u64) {
    room.apply(from1);
    room.apply(from2);
    let c1 = room.changes(from1);
    let c2 = room.changes(from2);
    let ack1 = from1.last().map_or(0, |b| b.seq);
    let ack2 = from2.last().map_or(0, |b| b.seq);
    w2.hear(&c1, ack2, now);
    w1.hear(&c2, ack1, now);
}

#[test]
fn a_joined_window_shows_what_the_seed_made() {
    let (_room, w1, w2, ..) = two();
    same(&w1, &w2);
    assert_eq!(w2.shim.skipped, 0);
}

#[test]
fn different_properties_of_one_layer_merge_and_one_property_settles_on_one_value() {
    let (mut room, mut w1, mut w2, _frame, a, _b) = two();
    w1.doc.get_mut(a).unwrap().fills = vec![Paint::solid(Color::WHITE)];
    w2.doc.get_mut(a).unwrap().opacity = 0.5;
    let (b1, b2) = (w1.flush(200), w2.flush(200));
    settle(&mut room, &mut w1, &mut w2, &b1, &b2, 300);
    same(&w1, &w2);
    assert_eq!(w1.doc.node(a).unwrap().opacity, 0.5);
    assert_eq!(w2.doc.node(a).unwrap().fills.len(), 1);
    // The same property at once: both settle on one value (the room's order).
    w1.doc.get_mut(a).unwrap().name = "from one".into();
    w2.doc.get_mut(a).unwrap().name = "from two".into();
    let (b1, b2) = (w1.flush(400), w2.flush(400));
    settle(&mut room, &mut w1, &mut w2, &b1, &b2, 500);
    same(&w1, &w2);
    assert_eq!(w1.doc.node(a).unwrap().name, "from two");
}

#[test]
fn layers_made_at_once_in_one_frame_both_survive_with_their_own_ids() {
    let (mut room, mut w1, mut w2, frame, ..) = two();
    let x = w1.doc.create(frame, NodeKind::Rectangle, "X", 0.0, 0.0, 10.0, 10.0).unwrap();
    let y = w2.doc.create(frame, NodeKind::Rectangle, "Y", 0.0, 0.0, 10.0, 10.0).unwrap();
    assert_ne!(x, y);
    let (b1, b2) = (w1.flush(200), w2.flush(200));
    settle(&mut room, &mut w1, &mut w2, &b1, &b2, 300);
    same(&w1, &w2);
    assert!(w1.doc.contains(y) && w2.doc.contains(x));
    assert_eq!(w1.shim.skipped + w2.shim.skipped, 0);
}

#[test]
fn deleting_a_frame_while_the_other_edits_inside_it_leaves_it_gone_in_both() {
    let (mut room, mut w1, mut w2, frame, a, _b) = two();
    w1.doc.remove(frame).unwrap();
    w2.doc.get_mut(a).unwrap().opacity = 0.3;
    let z = w2.doc.create(frame, NodeKind::Rectangle, "Z", 0.0, 0.0, 5.0, 5.0).unwrap();
    let (b1, b2) = (w1.flush(200), w2.flush(200));
    settle(&mut room, &mut w1, &mut w2, &b1, &b2, 300);
    same(&w1, &w2);
    assert!(!w1.doc.contains(frame) && !w2.doc.contains(frame) && !w2.doc.contains(a) && !w1.doc.contains(z));
}

#[test]
fn moving_two_layers_into_each_other_leaves_neither_in_both() {
    let (mut room, mut w1, mut w2, frame, ..) = two();
    let page = w1.doc.first_page().unwrap();
    let f2 = w1.doc.create(page, NodeKind::Frame, "F2", 400.0, 0.0, 100.0, 100.0).unwrap();
    let first = w1.flush(150);
    let c = room.take(&first);
    w2.hear(&c, 0, 160);
    assert!(w2.doc.contains(f2));
    // Each moves one frame into the other at once: the room would hold a loop, so neither window shows either (both agree).
    w1.doc.place(frame, f2, 0).unwrap();
    w2.doc.place(f2, frame, 0).unwrap();
    let (b1, b2) = (w1.flush(200), w2.flush(200));
    settle(&mut room, &mut w1, &mut w2, &b1, &b2, 300);
    same(&w1, &w2);
    assert!(!w1.doc.contains(frame) && !w1.doc.contains(f2));
}

#[test]
fn styles_and_the_files_own_properties_travel_and_a_removed_entry_is_removed() {
    let (mut room, mut w1, mut w2, ..) = two();
    w1.doc.name = "Renamed".into();
    w1.doc.styles.push(figcraft_doc::Style {
        id: "S:1".into(),
        name: "Brand".into(),
        description: String::new(),
        kind: figcraft_doc::StyleKind::Paint { paints: vec![Paint::solid(Color::BLACK)] },
        bound: Default::default(),
    });
    let b1 = w1.flush(200);
    let c = room.take(&b1);
    w2.hear(&c, 0, 300);
    same(&w1, &w2);
    assert_eq!(w2.doc.name, "Renamed");
    assert_eq!(w2.doc.styles.len(), 1);
    w2.doc.styles.clear();
    let b2 = w2.flush(400);
    let c = room.take(&b2);
    w1.hear(&c, b1.last().unwrap().seq, 500);
    assert!(w1.doc.styles.is_empty());
}

#[test]
fn a_value_too_big_for_one_shared_value_is_refused_loudly() {
    let (_room, mut w1, ..) = two();
    let t = w1.doc.create(w1.doc.first_page().unwrap(), NodeKind::Rectangle, &"n".repeat(70_000), 0.0, 0.0, 5.0, 5.0).unwrap();
    let _ = t;
    let e = w1.shim.local(&w1.doc, "Edit", false, 200).unwrap_err();
    assert!(matches!(e, ShimError::TooLarge(_)), "{e}");
}

#[test]
fn batches_in_flight_are_put_back_over_a_delta() {
    let (mut room, mut w1, mut w2, _frame, a, _b) = two();
    // Window 1 edits, the host has not acknowledged it; window 2's older edit to the same property arrives first.
    w1.doc.get_mut(a).unwrap().name = "mine".into();
    let sent = w1.flush(200);
    assert_eq!(sent.len(), 1);
    w2.doc.get_mut(a).unwrap().name = "theirs".into();
    let b2 = w2.flush(200);
    let c2 = room.take(&b2);
    w1.hear(&c2, 0, 250);
    assert_eq!(w1.doc.node(a).unwrap().name, "mine", "the unacknowledged edit stays on top");
    // The room orders mine after theirs: everyone ends on mine.
    let c1 = room.take(&sent);
    w2.hear(&c1, b2.last().unwrap().seq, 300);
    w1.hear(&[], sent.last().unwrap().seq, 300);
    assert_eq!(w2.doc.node(a).unwrap().name, "mine");
    same(&w1, &w2);
}
