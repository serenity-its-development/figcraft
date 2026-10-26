//! Design's live editing for the OneOS host bridge.
//!
//! The host (OneOS) keeps ONE replica of the file's shared document and speaks to the editor frame in the host bridge's
//! operation contract (`plans/mona/craft-integration-design-2026-10-25.md` section 12.3). FigCraft's own multiplayer (a server
//! that orders batches, links as access control) is NOT used: this crate maps FigCraft's property-level changes
//! ([`figcraft_collab`]: the synced object/property model, `diff` and `Replica::apply`) onto that contract and back. It is pure
//! and testable without a browser:
//!
//! * [`Shim::seed`] and [`Shim::local`] turn the editor's document into ops (`ensure`, `set`, `remove`);
//! * [`Shim::remote_state`] and [`Shim::remote_delta`] turn what the host sends back (`craft:crdt:state`, `craft:crdt:delta`) into
//!   the document to show.
//!
//! ★ WHAT IS SHARED, AND HOW (the schema beneath the doc's `craft` root). Every shared value is one JSON TEXT in a map key, so a
//! key is one property and two people changing different properties of one layer (its fill and its position) merge, while two
//! people changing one property settle on one value in every window (the room's last writer).
//!
//! | what | where | how |
//! |---|---|---|
//! | a layer | `n.<id>` | one key per property: `kind`, `parent` (null for a page), `position` (the fractional key that orders it among its siblings), then `name`, `fills`, `layout`, ... (every [`Prop`]) |
//! | the file's own properties | `d.<prop>` | name, current page, document colours, style/variable orders ... ([`DocProp`]) |
//! | styles, variables, collections, comment threads | `e.<kind>:<id>` | one key per entry ([`Entry`]), the whole entry as JSON |
//!
//! A layer is created by `ensure n.<id>` and its keys; deleting a layer removes its key and every key of what was inside it. What a
//! window shows is always a pure function of the shared tree ([`Shim`]'s reconcile): a layer is shown when it chains up to a page
//! through `parent` keys that exist. So a layer made in a frame another person deleted at the same time is gone in every window, and
//! two people moving two layers into each other leave neither (both windows agree).
//!
//! Not shared live: pictures larger than one shared value (about 60 KB; smaller ones travel with the document), the library a file
//! publishes, plugin settings, view state (zoom, selection). A layer property larger than one shared value is refused loudly
//! ([`ShimError::TooLarge`]) and the frame falls back to single-editor.
//!
//! The frame's own edits are not echoed back by the host, so the shim keeps its view of the shared tree itself: the host's truth
//! (`confirmed`) plus the local batches the host has not yet acknowledged (`pending`), re-applied over every delta (the contract's
//! client-side prediction). A batch counts as acknowledged when a message carries `ack >= seq`, or after [`PENDING_MS`].
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::{BTreeMap, BTreeSet};

use figcraft_collab::{Change, DocProp, Entry, Json, Mode, Prop, Replica, Target};
use figcraft_doc::{Document, NodeId};
use serde_json::{Map, Value, json};

/// A local batch is taken as applied by the host this long after it was sent, if no message acknowledged it sooner.
pub const PENDING_MS: u64 = 5_000;
/// At most this many local batches wait for an acknowledgement.
pub const MAX_PENDING: usize = 500;
/// The most ops one batch carries (the host's limit is 2000).
pub const BATCH_OPS: usize = 1_500;
/// The largest shared value (JSON text) the shim writes (the host's limit is 65 536 characters).
pub const MAX_VALUE: usize = 60_000;
/// Parent links followed before a chain is taken to be a cycle.
const MAX_DEPTH: usize = 10_000;

/// Why the shim could not do what was asked. It is loud: the frame says it and falls back to single-editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShimError {
    /// A piece of the document is bigger than one shared value holds.
    TooLarge(String),
    /// The shared tree and the shim disagree (an op did not apply).
    Tree(String),
    /// FigCraft's replica refused a change.
    Sync(String),
}

impl std::fmt::Display for ShimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShimError::TooLarge(m) | ShimError::Tree(m) | ShimError::Sync(m) => f.write_str(m),
        }
    }
}

impl From<figcraft_collab::Error> for ShimError {
    fn from(e: figcraft_collab::Error) -> Self {
        ShimError::Sync(e.to_string())
    }
}

/// One batch of ops for `craft:crdt:ops`.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub seq: u64,
    pub ops: Vec<Value>,
    /// The app's own name for the edit (its caption in the room's history).
    pub label: String,
    /// Fold into the previous undo step (a drag or typing burst).
    pub join: bool,
    /// The first batch of a seed (`seed: true`).
    pub seed: bool,
}

/// A change the host reports: set the tree at `path` (below `doc`, which the caller strips) to `value`, or remove it.
#[derive(Debug, Clone, PartialEq)]
pub struct HostChange {
    pub path: Vec<String>,
    pub value: Option<Value>,
}

/// A host message's `changes` array.
#[must_use]
pub fn changes_of(v: &Value) -> Vec<HostChange> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let path: Vec<String> = c.get("path")?.as_array()?.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect();
                    let removed = c.get("removed").and_then(Value::as_bool) == Some(true);
                    Some(HostChange { path, value: if removed { None } else { c.get("value").cloned() } })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ── the shared tree, as a JSON value ────────────────────────────────────────────────────────────────────────────────

fn segment_at<'a>(root: &'a mut Value, path: &[String]) -> Option<&'a mut Value> {
    let mut at = root;
    for seg in path {
        at = at.as_object_mut()?.get_mut(seg)?;
    }
    Some(at)
}

fn path_of(op: &Value) -> Vec<String> {
    op.get("path").and_then(Value::as_array).map(|p| p.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

/// Apply one op of the contract to a tree (`ensure`, `new`, `set`, `remove`: the shim only writes maps).
fn apply_op(root: &mut Value, op: &Value) -> Result<(), ShimError> {
    let kind = op.get("op").and_then(Value::as_str).unwrap_or_default();
    let path = path_of(op);
    let key = op.get("key").and_then(Value::as_str).ok_or_else(|| ShimError::Tree(format!("{kind}: no key")))?;
    let node = segment_at(root, &path).ok_or_else(|| ShimError::Tree(format!("craft-path-missing: nothing is at {}", path.join("/"))))?;
    let map = node.as_object_mut().ok_or_else(|| ShimError::Tree(format!("craft-path-kind: {} is not a map", path.join("/"))))?;
    match kind {
        "set" => {
            map.insert(key.to_owned(), op.get("value").cloned().ok_or_else(|| ShimError::Tree("set: no value".into()))?);
        }
        "remove" => {
            map.remove(key);
        }
        "new" => {
            map.insert(key.to_owned(), Value::Object(Map::new()));
        }
        "ensure" => {
            map.entry(key.to_owned()).or_insert_with(|| Value::Object(Map::new()));
        }
        other => return Err(ShimError::Tree(format!("no op {other}"))),
    }
    Ok(())
}

/// Set (or remove) the value at `path` of a host change.
fn apply_change(root: &mut Value, path: &[String], value: Option<&Value>) -> Result<(), ShimError> {
    let Some((leaf, parents)) = path.split_last() else {
        if let Some(v) = value {
            *root = if v.is_object() { v.clone() } else { Value::Object(Map::new()) };
        }
        return Ok(());
    };
    let mut at = root;
    for seg in parents {
        let obj = at.as_object_mut().ok_or_else(|| ShimError::Tree(format!("{seg}: not a map")))?;
        at = obj.entry(seg.clone()).or_insert_with(|| Value::Object(Map::new()));
        if !at.is_object() {
            *at = Value::Object(Map::new());
        }
    }
    let obj = at.as_object_mut().ok_or_else(|| ShimError::Tree("not a map".into()))?;
    match value {
        Some(v) => {
            obj.insert(leaf.clone(), v.clone());
        }
        None => {
            obj.remove(leaf);
        }
    }
    Ok(())
}

fn op_ensure(path: &[&str], key: &str) -> Value {
    json!({"op": "ensure", "path": path, "key": key, "kind": "map"})
}
fn op_set(path: &[&str], key: &str, text: &str) -> Value {
    json!({"op": "set", "path": path, "key": key, "value": text})
}
fn op_remove(path: &[&str], key: &str) -> Value {
    json!({"op": "remove", "path": path, "key": key})
}

// ── names: properties, document properties and entries as keys ──────────────────────────────────────────────────────

fn prop_key(p: Prop) -> String {
    serde_json::to_value(p).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}
fn parse_prop(key: &str) -> Option<Prop> {
    serde_json::from_value(Value::String(key.to_owned())).ok()
}
fn doc_prop_key(p: DocProp) -> String {
    serde_json::to_value(p).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}
fn parse_doc_prop(key: &str) -> Option<DocProp> {
    serde_json::from_value(Value::String(key.to_owned())).ok()
}
fn entry_key(e: &Entry) -> String {
    match e {
        Entry::Style(id) => format!("style:{id}"),
        Entry::Comment(id) => format!("comment:{id}"),
        Entry::Collection(id) => format!("collection:{id}"),
        Entry::Variable(id) => format!("variable:{id}"),
        Entry::DevSnapshot(id) => format!("devSnapshot:{id}"),
        Entry::Image(hash) => format!("image:{hash}"),
    }
}
fn parse_entry(key: &str) -> Option<Entry> {
    let (kind, id) = key.split_once(':')?;
    Some(match kind {
        "style" => Entry::Style(id.to_owned()),
        "comment" => Entry::Comment(id.parse().ok()?),
        "collection" => Entry::Collection(id.to_owned()),
        "variable" => Entry::Variable(id.to_owned()),
        "devSnapshot" => Entry::DevSnapshot(id.parse().ok()?),
        "image" => Entry::Image(id.to_owned()),
        _ => return None,
    })
}

fn text_of(v: &Json) -> String {
    v.0.to_string()
}
fn json_of(text: &str) -> Option<Json> {
    serde_json::from_str(text).ok().map(Json)
}
fn node_key(id: NodeId) -> String {
    id.0.to_string()
}
fn parse_node(key: &str) -> Option<NodeId> {
    key.parse::<u64>().ok().map(NodeId)
}

/// The layer map of `id` in `tree`.
fn layer<'a>(tree: &'a Value, id: NodeId) -> Option<&'a Map<String, Value>> {
    tree.get("n")?.get(node_key(id))?.as_object()
}

/// `Some(None)`: a page (`parent` null); `Some(Some(p))`: its parent; `None`: no usable `parent` key.
fn parent_of(m: &Map<String, Value>) -> Option<Option<NodeId>> {
    let j = json_of(m.get("parent")?.as_str()?)?;
    match j.0 {
        Value::Null => Some(None),
        Value::Number(n) => n.as_u64().map(|n| Some(NodeId(n))),
        _ => None,
    }
}

/// Whether `id` chains up to a page through layers the tree holds: the one rule that decides what a window shows.
fn reachable(tree: &Value, id: NodeId) -> bool {
    let mut cur = id;
    for _ in 0..MAX_DEPTH {
        let Some(m) = layer(tree, cur) else { return false };
        match parent_of(m) {
            Some(None) => return true,
            Some(Some(p)) => cur = p,
            None => return false,
        }
    }
    false
}

fn depth_of(tree: &Value, id: NodeId) -> usize {
    let (mut cur, mut d) = (id, 0);
    while let Some(Some(p)) = layer(tree, cur).and_then(parent_of) {
        d += 1;
        cur = p;
        if d > MAX_DEPTH {
            break;
        }
    }
    d
}

/// What a reconcile looks at: `None` is everything of that kind.
#[derive(Default)]
struct Touch {
    nodes: Option<BTreeSet<NodeId>>,
    docs: Option<BTreeSet<String>>,
    entries: Option<BTreeSet<String>>,
}

impl Touch {
    fn none() -> Touch {
        Touch { nodes: Some(BTreeSet::new()), docs: Some(BTreeSet::new()), entries: Some(BTreeSet::new()) }
    }
    fn note(&mut self, path: &[String]) {
        match path.first().map(String::as_str) {
            None => *self = Touch::default(),
            Some("n") => match path.get(1).and_then(|k| parse_node(k)) {
                Some(id) => {
                    if let Some(s) = self.nodes.as_mut() {
                        s.insert(id);
                    }
                }
                None => self.nodes = None,
            },
            Some("d") => match path.get(1) {
                Some(k) => {
                    if let Some(s) = self.docs.as_mut() {
                        s.insert(k.clone());
                    }
                }
                None => self.docs = None,
            },
            Some("e") => match path.get(1) {
                Some(k) => {
                    if let Some(s) = self.entries.as_mut() {
                        s.insert(k.clone());
                    }
                }
                None => self.entries = None,
            },
            _ => {}
        }
    }
}

type Pending = (u64, u64, Vec<Value>);

/// One window's side of the live document.
pub struct Shim {
    client: u32,
    replica: Option<Replica>,
    /// The shared tree as this window expects it: the host's truth plus the batches still in flight.
    view: Value,
    /// The host's truth alone, kept while batches are in flight.
    confirmed: Option<Value>,
    pending: Vec<Pending>,
    seq: u64,
    /// Changes the replica could not apply (it skips them identically in every window). Stays 0 in a healthy room.
    pub skipped: usize,
}

impl Shim {
    /// A shim whose window makes layer ids in the space of `client` (not 0; two windows of one room must differ).
    #[must_use]
    pub fn new(client: u32) -> Self {
        Shim { client, replica: None, view: json!({}), confirmed: None, pending: Vec::new(), seq: 0, skipped: 0 }
    }

    /// The client id the window's document makes ids with.
    #[must_use]
    pub fn client(&self) -> u32 {
        self.client
    }

    /// The document the shared tree currently says (what the editor shows once the caller installs it).
    #[must_use]
    pub fn document(&self) -> Option<&Document> {
        self.replica.as_ref().map(Replica::document)
    }

    /// Start from `doc` when nothing was shared yet.
    fn attach(&mut self, doc: &Document) -> &mut Replica {
        self.replica.get_or_insert_with(|| {
            let mut d = doc.clone();
            d.set_client_id(self.client);
            Replica::new(d)
        })
    }

    // ── seed and local edits ──────────────────────────────────────────────────────────────────────────────────────

    /// ★ SEED: the whole document as ops. The first batch is the seed (`seed: true`, one untracked change on the host); the rest
    /// follow as ordinary batches.
    pub fn seed(&mut self, doc: &Document, now_ms: u64) -> Result<Vec<Batch>, ShimError> {
        self.view = json!({});
        self.confirmed = None;
        self.pending.clear();
        let mut d = doc.clone();
        d.set_client_id(self.client);
        let replica = Replica::new(d);
        let changes = replica.snapshot()?;
        self.replica = Some(replica);
        let mut ops = vec![op_ensure(&[], "n"), op_ensure(&[], "d"), op_ensure(&[], "e")];
        for op in &ops {
            apply_op(&mut self.view, op)?;
        }
        let old = Document::without_pages("");
        let mut ensured = BTreeSet::new();
        for c in &changes {
            self.ops_for(c, &old, &mut ensured, &mut ops)?;
        }
        self.emit(ops, "Opened the file", false, true, now_ms)
    }

    /// ★ A LOCAL EDIT: the batches that take what the peers hold to `doc`, or none when it is the same. `join`: the edit continues
    /// a gesture or typing burst, so it folds into the previous undo step.
    pub fn local(&mut self, doc: &Document, label: &str, join: bool, now_ms: u64) -> Result<Vec<Batch>, ShimError> {
        if self.replica.is_none() {
            self.attach(doc);
            return Ok(Vec::new());
        }
        let Some(replica) = self.replica.as_ref() else { return Ok(Vec::new()) };
        let d = figcraft_collab::diff(replica, doc)?;
        if d.changes.is_empty() {
            return Ok(Vec::new());
        }
        let mut ops = Vec::new();
        for root in ["n", "d", "e"] {
            if self.view.get(root).is_none() {
                ops.push(op_ensure(&[], root));
            }
        }
        let old = replica.document().clone();
        let mut ensured = BTreeSet::new();
        for c in &d.changes {
            self.ops_for(c, &old, &mut ensured, &mut ops)?;
        }
        self.replica = Some(d.replica);
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        self.emit(ops, label, join, false, now_ms)
    }

    /// The ops one change makes. `old` is the document before it (a deleted layer's subtree is what it held).
    fn ops_for(&self, change: &Change, old: &Document, ensured: &mut BTreeSet<NodeId>, out: &mut Vec<Value>) -> Result<(), ShimError> {
        let held = |path: &[&str], key: &str, text: &str| -> bool {
            let mut at = &self.view;
            for seg in path {
                match at.get(*seg) {
                    Some(v) => at = v,
                    None => return false,
                }
            }
            at.get(key).and_then(Value::as_str) == Some(text)
        };
        let checked = |what: &str, text: String| -> Result<String, ShimError> {
            if text.len() > MAX_VALUE {
                Err(ShimError::TooLarge(format!("{what} is {} bytes, and the room holds at most {MAX_VALUE} in one value", text.len())))
            } else {
                Ok(text)
            }
        };
        match change {
            Change::Create { id, kind, parent, position, props } => {
                let k = node_key(*id);
                ensured.insert(*id);
                out.push(op_ensure(&["n"], &k));
                let path = ["n", k.as_str()];
                out.push(op_set(&path, "kind", &checked("a layer's type", text_of(kind))?));
                out.push(op_set(&path, "parent", &text_of(&Json::of(parent)?)));
                out.push(op_set(&path, "position", &text_of(&Json::of(position)?)));
                for (p, v) in props {
                    out.push(op_set(&path, &prop_key(*p), &checked(&format!("layer {id} {}", prop_key(*p)), text_of(v))?));
                }
            }
            Change::Set { id, prop, value } => {
                let k = node_key(*id);
                let text = checked(&format!("layer {id} {}", prop_key(*prop)), text_of(value))?;
                if !held(&["n", k.as_str()], &prop_key(*prop), &text) {
                    // A layer another person deleted at the same time may be gone from this window's copy of the room: the
                    // write makes its container again (without a parent, so the layer stays deleted), never a refused batch.
                    if ensured.insert(*id) {
                        out.push(op_ensure(&["n"], &k));
                    }
                    out.push(op_set(&["n", k.as_str()], &prop_key(*prop), &text));
                }
            }
            Change::Delete { id } => {
                for gone in old.subtree(*id) {
                    out.push(op_remove(&["n"], &node_key(gone)));
                }
            }
            Change::SetDoc { prop, value } => {
                let text = checked(&format!("the file's {}", doc_prop_key(*prop)), text_of(value))?;
                if !held(&["d"], &doc_prop_key(*prop), &text) {
                    out.push(op_set(&["d"], &doc_prop_key(*prop), &text));
                }
            }
            Change::Put { entry, value } => {
                let text = text_of(value);
                // A picture bigger than one shared value stays in this window (and the saved file).
                if text.len() > MAX_VALUE && matches!(entry, Entry::Image(_)) {
                    return Ok(());
                }
                let text = checked(&entry.describe(), text)?;
                if !held(&["e"], &entry_key(entry), &text) {
                    out.push(op_set(&["e"], &entry_key(entry), &text));
                }
            }
            Change::Remove { entry } => out.push(op_remove(&["e"], &entry_key(entry))),
        }
        Ok(())
    }

    /// Chop `ops` into batches, apply them to the view and remember them until the host acknowledges.
    fn emit(&mut self, ops: Vec<Value>, label: &str, join: bool, seed: bool, now_ms: u64) -> Result<Vec<Batch>, ShimError> {
        if self.confirmed.is_none() {
            self.confirmed = Some(self.view.clone());
        }
        let mut batches = Vec::new();
        for (i, chunk) in ops.chunks(BATCH_OPS).enumerate() {
            for op in chunk {
                apply_op(&mut self.view, op)?;
            }
            self.seq += 1;
            self.pending.push((self.seq, now_ms, chunk.to_vec()));
            batches.push(Batch { seq: self.seq, ops: chunk.to_vec(), label: label.to_owned(), join: join || i > 0, seed: seed && i == 0 });
        }
        self.prune(now_ms);
        Ok(batches)
    }

    /// Batches the host has had long enough count as applied.
    fn prune(&mut self, now_ms: u64) {
        while self.pending.len() > MAX_PENDING || self.pending.first().is_some_and(|(_, at, _)| at + PENDING_MS <= now_ms) {
            let (_, _, ops) = self.pending.remove(0);
            if let Some(truth) = self.confirmed.as_mut() {
                for op in &ops {
                    let _ = apply_op(truth, op);
                }
            }
        }
        if self.pending.is_empty() {
            self.confirmed = None;
        }
    }

    // ── what the host sends ───────────────────────────────────────────────────────────────────────────────────────

    /// The whole shared state arrived (a join, a resync after a refusal, a snapshot): show it. Batches waiting for an
    /// acknowledgement are dropped - the state is the host's truth. `doc` is what the window shows now (used the first time).
    pub fn remote_state(&mut self, craft: &Value, doc: &Document) -> Result<bool, ShimError> {
        self.view = if craft.is_object() { craft.clone() } else { json!({}) };
        self.confirmed = None;
        self.pending.clear();
        self.attach(doc);
        self.show(&Touch::default())
    }

    /// ★ A REMOTE CHANGE arrived: apply the host's `changes` (and `ack`: the last local batch it applied), put the batches still in
    /// flight back on top, and show what changed. Answers whether the document changed. The caller sent its own unsent edits
    /// ([`Shim::local`]) first.
    pub fn remote_delta(&mut self, changes: &[HostChange], ack: u64, now_ms: u64, doc: &Document) -> Result<bool, ShimError> {
        self.attach(doc);
        let mut touch = Touch::none();
        for c in changes {
            touch.note(&c.path);
        }
        match self.confirmed.take() {
            Some(mut truth) => {
                // The host applied our batches up to `ack` before it read what it reports.
                while self.pending.first().is_some_and(|(seq, _, _)| *seq <= ack) {
                    let (_, _, ops) = self.pending.remove(0);
                    for op in &ops {
                        let _ = apply_op(&mut truth, op);
                    }
                }
                for c in changes {
                    apply_change(&mut truth, &c.path, c.value.as_ref())?;
                }
                let mut view = truth.clone();
                for (_, _, ops) in &self.pending {
                    for op in ops {
                        touch.note(&{
                            let mut p = path_of(op);
                            if let Some(k) = op.get("key").and_then(Value::as_str) {
                                p.push(k.to_owned());
                            }
                            p
                        });
                        let _ = apply_op(&mut view, op);
                    }
                }
                self.view = view;
                self.confirmed = if self.pending.is_empty() { None } else { Some(truth) };
            }
            None => {
                for c in changes {
                    apply_change(&mut self.view, &c.path, c.value.as_ref())?;
                }
            }
        }
        self.prune(now_ms);
        self.show(&touch)
    }

    /// Bring the replica to what the view says, for the touched keys. Answers whether anything changed.
    fn show(&mut self, touch: &Touch) -> Result<bool, ShimError> {
        let Some(replica) = self.replica.as_ref() else { return Ok(false) };
        let changes = reconcile(&self.view, replica, touch)?;
        if changes.is_empty() {
            return Ok(false);
        }
        let mut next = replica.clone();
        let applied = next.apply(&changes, Mode::Lenient)?;
        self.skipped += applied.skipped;
        self.replica = Some(next);
        Ok(true)
    }
}

/// The changes that take `replica` to the layers and entries `tree` holds, for what `touch` names.
fn reconcile(tree: &Value, replica: &Replica, touch: &Touch) -> Result<Vec<Change>, ShimError> {
    let doc = replica.document();
    let mut ids: BTreeSet<NodeId> = match &touch.nodes {
        Some(s) => s.clone(),
        None => {
            let mut all: BTreeSet<NodeId> = tree.get("n").and_then(Value::as_object).map(|m| m.keys().filter_map(|k| parse_node(k)).collect()).unwrap_or_default();
            all.extend(doc.nodes().map(|n| n.id));
            all
        }
    };
    // A layer that appears brings the layers beneath it that were already waiting for it.
    if touch.nodes.is_some() && ids.iter().any(|id| !doc.contains(*id) && reachable(tree, *id)) {
        let mut kids: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
        if let Some(n) = tree.get("n").and_then(Value::as_object) {
            for (k, v) in n {
                if let (Some(id), Some(Some(p))) = (parse_node(k), v.as_object().and_then(parent_of)) {
                    kids.entry(p).or_default().push(id);
                }
            }
        }
        let mut queue: Vec<NodeId> = ids.iter().copied().filter(|id| !doc.contains(*id)).collect();
        while let Some(id) = queue.pop() {
            for c in kids.get(&id).into_iter().flatten() {
                if ids.insert(*c) {
                    queue.push(*c);
                }
            }
        }
    }
    let mut creates: Vec<(usize, NodeId)> = Vec::new();
    let mut sets: Vec<Change> = Vec::new();
    let mut deletes: Vec<Change> = Vec::new();
    for id in ids {
        let there = doc.contains(id);
        if !reachable(tree, id) {
            if there {
                deletes.push(Change::Delete { id });
            }
            continue;
        }
        let Some(m) = layer(tree, id) else { continue };
        if !there {
            creates.push((depth_of(tree, id), id));
            continue;
        }
        // Parent, then position, then the rest: the order `diff` writes them in.
        let mut keys: Vec<(&String, Prop)> = m.keys().filter_map(|k| parse_prop(k).map(|p| (k, p))).collect();
        keys.sort_by_key(|(_, p)| match p {
            Prop::Parent => 0,
            Prop::Position => 1,
            _ => 2,
        });
        for (k, p) in keys {
            let Some(want) = m.get(k).and_then(Value::as_str).and_then(json_of) else { continue };
            let have = replica.value(&Target::Node(id, p))?;
            if have.as_ref() != Some(&want) {
                sets.push(Change::Set { id, prop: p, value: want });
            }
        }
    }
    creates.sort();
    let mut out: Vec<Change> = Vec::new();
    for (_, id) in creates {
        let Some(m) = layer(tree, id) else { continue };
        let read = |k: &str| m.get(k).and_then(Value::as_str).and_then(json_of);
        let (Some(kind), Some(parent), Some(position)) = (read("kind"), read("parent"), read("position")) else { continue };
        let Ok(parent) = parent.read::<Option<NodeId>>("parent") else { continue };
        let Ok(position) = position.read::<String>("position") else { continue };
        let mut props = Vec::new();
        for (k, v) in m {
            if let (Some(p), Some(j)) = (parse_prop(k), v.as_str().and_then(json_of))
                && !matches!(p, Prop::Kind | Prop::Parent | Prop::Position)
            {
                props.push((p, j));
            }
        }
        out.push(Change::Create { id, kind, parent, position, props });
    }
    out.extend(sets);
    out.extend(deletes);
    // The file's own properties.
    let d = tree.get("d").and_then(Value::as_object);
    let keys: BTreeSet<String> = match &touch.docs {
        Some(s) => s.clone(),
        None => d.map(|m| m.keys().cloned().collect()).unwrap_or_default(),
    };
    for k in keys {
        let (Some(p), Some(want)) = (parse_doc_prop(&k), d.and_then(|m| m.get(&k)).and_then(Value::as_str).and_then(json_of)) else { continue };
        let have = replica.value(&Target::Doc(p))?;
        if have.as_ref() != Some(&want) {
            out.push(Change::SetDoc { prop: p, value: want });
        }
    }
    // Styles, variables, collections and comment threads.
    let e = tree.get("e").and_then(Value::as_object);
    let mut keys: BTreeSet<String> = match &touch.entries {
        Some(s) => s.clone(),
        None => e.map(|m| m.keys().cloned().collect()).unwrap_or_default(),
    };
    if touch.entries.is_none() {
        // A whole state: what this window holds that the room does not (except pictures) is gone from the room.
        for c in replica.snapshot()? {
            if let Change::Put { entry, .. } = c
                && !matches!(entry, Entry::Image(_))
            {
                keys.insert(entry_key(&entry));
            }
        }
    }
    for k in keys {
        let Some(entry) = parse_entry(&k) else { continue };
        let want = e.and_then(|m| m.get(&k)).and_then(Value::as_str).and_then(json_of);
        let have = replica.value(&Target::Entry(entry.clone()))?;
        match (want, have) {
            (Some(w), h) if h.as_ref() != Some(&w) => out.push(Change::Put { entry, value: w }),
            (None, Some(_)) => out.push(Change::Remove { entry }),
            _ => {}
        }
    }
    Ok(out)
}

/// What a document shows of what the shim shares, as one comparable JSON value: the pages in order, every layer's properties and
/// children, the file's properties and every entry. Two windows of one room show the same digest.
pub fn digest(doc: &Document) -> Result<Value, ShimError> {
    let replica = Replica::new(doc.clone());
    let mut layers = Map::new();
    for n in replica.document().nodes() {
        let mut m = Map::new();
        m.insert("children".into(), json!(n.children));
        m.insert("parent".into(), json!(n.parent));
        for p in std::iter::once(&Prop::Kind).chain(Prop::FIELDS.iter().filter(|p| **p != Prop::Kind)) {
            if let Some(v) = replica.value(&Target::Node(n.id, *p))? {
                m.insert(prop_key(*p), v.0);
            }
        }
        layers.insert(node_key(n.id), Value::Object(m));
    }
    let mut entries = Map::new();
    let mut props = Map::new();
    for c in replica.snapshot()? {
        match c {
            Change::Put { entry, value } if !matches!(entry, Entry::Image(_)) => {
                entries.insert(entry_key(&entry), value.0);
            }
            Change::SetDoc { prop, value } => {
                props.insert(doc_prop_key(prop), value.0);
            }
            _ => {}
        }
    }
    Ok(json!({"pages": replica.document().pages, "layers": layers, "props": props, "entries": entries}))
}

#[cfg(test)]
mod tests;
