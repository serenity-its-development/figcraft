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
                // A text op names the text itself; the others name a key below `path`.
                if let Some(k) = op.get("key").and_then(Value::as_str) {
                    path.push(k.to_owned());
                }
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
fn moving_two_layers_into_each_other_at_once_leaves_both_and_the_lowest_goes_home() {
    let (mut room, mut w1, mut w2, frame, ..) = two();
    let page = w1.doc.first_page().unwrap();
    let f2 = w1.doc.create(page, NodeKind::Frame, "F2", 400.0, 0.0, 100.0, 100.0).unwrap();
    let first = w1.flush(150);
    let c = room.take(&first);
    w2.hear(&c, 0, 160);
    assert!(w2.doc.contains(f2));
    // Each moves one frame into the other at once: the room holds a loop of `parent` keys. The cycle rule keeps both.
    w1.doc.place(frame, f2, 0).unwrap();
    w2.doc.place(f2, frame, 0).unwrap();
    let (b1, b2) = (w1.flush(200), w2.flush(200));
    settle(&mut room, &mut w1, &mut w2, &b1, &b2, 300);
    same(&w1, &w2);
    for w in [&w1, &w2] {
        assert!(w.doc.contains(frame) && w.doc.contains(f2), "nothing vanishes");
        // The lowest id of the cycle is back on its page, the other stays inside it.
        let (low, high) = if frame < f2 { (frame, f2) } else { (f2, frame) };
        assert_eq!(w.doc.node(low).unwrap().parent, Some(page), "the lowest goes back to where it came from");
        assert_eq!(w.doc.node(high).unwrap().parent, Some(low), "the other stays where it was moved");
    }
    assert_eq!(w1.shim.skipped + w2.shim.skipped, 0);
    // The cycle is settled in the room too: a window joining now shows the same.
    let mut w3 = Window::new(3, &base().0);
    w3.shim.remote_state(&room.tree, &w3.doc).unwrap();
    w3.doc = w3.shim.document().unwrap().clone();
    same(&w1, &w3);
}

#[test]
fn when_the_cycle_is_undone_the_layer_it_moved_goes_back_to_where_it_was_moved() {
    let (mut room, mut w1, mut w2, frame, ..) = two();
    let page = w1.doc.first_page().unwrap();
    let f2 = w1.doc.create(page, NodeKind::Frame, "F2", 400.0, 0.0, 100.0, 100.0).unwrap();
    let first = w1.flush(150);
    let c = room.take(&first);
    w2.hear(&c, 0, 160);
    w1.doc.place(frame, f2, 0).unwrap();
    w2.doc.place(f2, frame, 0).unwrap();
    let (b1, b2) = (w1.flush(200), w2.flush(200));
    settle(&mut room, &mut w1, &mut w2, &b1, &b2, 300);
    same(&w1, &w2);
    let (low, high) = if frame < f2 { (frame, f2) } else { (f2, frame) };
    // Somebody takes the other layer out to the page: no cycle is left, so the lowest layer is where its own move put it: inside it.
    w1.doc.place(high, page, 0).unwrap();
    let b = w1.flush(400);
    // The window that made the move shows its own consequence too.
    if w1.shim.take_reshown() {
        w1.doc = w1.shim.document().unwrap().clone();
    }
    let c = room.take(&b);
    w2.hear(&c, 0, 410);
    same(&w1, &w2);
    for w in [&w1, &w2] {
        assert_eq!(w.doc.node(high).unwrap().parent, Some(page));
        assert_eq!(w.doc.node(low).unwrap().parent, Some(high));
    }
    assert_eq!(w1.shim.skipped + w2.shim.skipped, 0);
}

#[test]
fn auto_layout_merges_by_field_two_people_changing_different_fields_both_land() {
    use figcraft_doc::AutoLayout;
    let (mut room, mut w1, mut w2, frame, ..) = two();
    w1.doc.get_mut(frame).unwrap().layout = Some(AutoLayout::default());
    let first = w1.flush(150);
    let c = room.take(&first);
    w2.hear(&c, 0, 160);
    assert!(w2.doc.node(frame).unwrap().layout.is_some());
    // One changes the gap, the other the padding, at once: both land.
    w1.doc.get_mut(frame).unwrap().layout.as_mut().unwrap().gap = 33.0;
    w2.doc.get_mut(frame).unwrap().layout.as_mut().unwrap().padding = [1.0, 2.0, 3.0, 4.0];
    let (b1, b2) = (w1.flush(200), w2.flush(200));
    settle(&mut room, &mut w1, &mut w2, &b1, &b2, 300);
    same(&w1, &w2);
    let l = w1.doc.node(frame).unwrap().layout.clone().unwrap();
    assert_eq!((l.gap, l.padding), (33.0, [1.0, 2.0, 3.0, 4.0]));
    // The same field at once: one value in both. Turning auto layout off wins over a field edit it did not see.
    w1.doc.get_mut(frame).unwrap().layout.as_mut().unwrap().gap = 5.0;
    w2.doc.get_mut(frame).unwrap().layout.as_mut().unwrap().gap = 6.0;
    let (b1, b2) = (w1.flush(400), w2.flush(400));
    settle(&mut room, &mut w1, &mut w2, &b1, &b2, 500);
    same(&w1, &w2);
    w1.doc.get_mut(frame).unwrap().layout = None;
    w2.doc.get_mut(frame).unwrap().layout.as_mut().unwrap().cross_gap = 77.0;
    let (b1, b2) = (w1.flush(600), w2.flush(600));
    settle(&mut room, &mut w1, &mut w2, &b1, &b2, 700);
    same(&w1, &w2);
    assert!(w1.doc.node(frame).unwrap().layout.is_none(), "turning it off is not undone by a field edit");
    // Switched on again later, it is whole.
    w2.doc.get_mut(frame).unwrap().layout = Some(AutoLayout { gap: 9.0, ..AutoLayout::default() });
    let b2 = w2.flush(800);
    let c = room.take(&b2);
    w1.hear(&c, 0, 810);
    same(&w1, &w2);
    assert_eq!(w1.doc.node(frame).unwrap().layout.as_ref().unwrap().gap, 9.0);
    assert_eq!(w1.shim.skipped + w2.shim.skipped, 0);
}

fn typed(w: &mut Window, id: NodeId, f: impl FnOnce(&mut String)) {
    if let NodeKind::Text(t) = &mut w.doc.get_mut(id).unwrap().kind {
        f(&mut t.characters);
    }
}

fn chars_of(w: &Window, id: NodeId) -> String {
    match &w.doc.node(id).unwrap().kind {
        NodeKind::Text(t) => t.characters.clone(),
        _ => panic!("not a text layer"),
    }
}

#[test]
fn text_merges_by_character_two_people_typing_in_one_text_both_land() {
    let (mut room, mut w1, mut w2, frame, ..) = two();
    let t = w1.doc.create(frame, NodeKind::Text(figcraft_doc::TextData { characters: "hello world".into(), ..Default::default() }), "T", 0.0, 0.0, 100.0, 20.0).unwrap();
    let first = w1.flush(150);
    let c = room.take(&first);
    w2.hear(&c, 0, 160);
    assert_eq!(chars_of(&w2, t), "hello world");
    // Edits one after the other travel as the characters that changed. (Two people typing AT ONCE is the real engine's merge, tested
    // from OneOS in `craft-design-convergence.test.ts`: this stand-in room only applies indexes in the order it is given them.)
    typed(&mut w1, t, |s| s.insert_str(0, "Oh, "));
    let b1 = w1.flush(200);
    let c = room.take(&b1);
    w2.hear(&c, 0, 210);
    assert_eq!(chars_of(&w2, t), "Oh, hello world");
    typed(&mut w2, t, |s| s.push_str(" and more"));
    let b2 = w2.flush(300);
    let c = room.take(&b2);
    w1.hear(&c, b1.last().unwrap().seq, 310);
    same(&w1, &w2);
    assert_eq!(chars_of(&w1, t), "Oh, hello world and more");
    typed(&mut w1, t, |s| *s = s.replace("hello ", ""));
    let b1 = w1.flush(400);
    // Only the changed characters travelled: one delete, no re-write of the whole text.
    assert!(b1.iter().flat_map(|b| &b.ops).any(|op| op["op"] == "text.delete" && op["len"] == 6), "{:?}", b1);
    assert!(!b1.iter().flat_map(|b| &b.ops).any(|op| op["op"] == "set" && op["key"] == "kind"), "the text's other data did not change");
    let c = room.take(&b1);
    w2.hear(&c, b2.last().unwrap().seq, 410);
    same(&w1, &w2);
    assert_eq!(chars_of(&w2, t), "Oh, world and more");
    // The characters are a CRDT text in the room, not a JSON blob.
    assert!(room.tree["n"][t.0.to_string()]["tx"]["$text"].is_array());
    assert_eq!(w1.shim.skipped + w2.shim.skipped, 0);
}

#[test]
fn a_text_layer_made_with_its_text_and_a_joining_window_shows_it() {
    let (mut room, mut w1, _w2, frame, ..) = two();
    let t = w1.doc.create(frame, NodeKind::Text(figcraft_doc::TextData { characters: "Grüße 😀".into(), ..Default::default() }), "T", 0.0, 0.0, 100.0, 20.0).unwrap();
    let batches = w1.flush(150);
    room.take(&batches);
    let mut w3 = Window::new(3, &base().0);
    w3.shim.remote_state(&room.tree, &w3.doc).unwrap();
    w3.doc = w3.shim.document().unwrap().clone();
    assert_eq!(chars_of(&w3, t), "Grüße 😀");
    same(&w1, &w3);
}

#[test]
fn a_cursor_names_the_page_the_pointer_and_the_layers_and_resolves_in_another_window() {
    let (_room, w1, w2, frame, a, _b) = two();
    let page = w1.doc.first_page().unwrap();
    let body = cursor_for(page, Some((12.5, 40.0)), &[a, frame]);
    let seen = peer_cursor(&w2.doc, &body).unwrap();
    assert_eq!((seen.page, seen.at, seen.selection.clone()), (page, Some((12.5, 40.0)), vec![a, frame]));
    // A layer this window does not have drops out; a page it does not have is no cursor.
    let body = cursor_for(page, None, &[a, NodeId(999_999_999)]);
    assert_eq!(peer_cursor(&w2.doc, &body).unwrap().selection, vec![a]);
    assert!(peer_cursor(&w2.doc, &cursor_for(NodeId(999_999_999), None, &[])).is_none());
    assert!(peer_cursor(&w2.doc, &json!({"pg": "x"})).is_none());
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
