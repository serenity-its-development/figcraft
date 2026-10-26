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
//! | a layer | `n.<id>` | one key per property: `kind`, `parent` (null for a page), `position` (the fractional key that orders it among its siblings), then `name`, `fills`, ... (every [`Prop`]) |
//! | auto layout, a layer's layout inside its parent | `n.<id>.layout`, `n.<id>.layout.<field>`, `childLayout`, `childLayout.<field>` | the property is a marker (`null`: no auto layout; `{}`: it has) and one key per FIELD (`gap`, `padding`, `mainAlign`, ...), so two people changing different fields of one layout both land |
//! | a text layer's characters | `n.<id>.tx` | a CRDT TEXT (the host's native text type); `kind` keeps the rest of the text data (style runs, alignment) with empty `characters`, so two people typing in one text merge character by character |
//! | where a layer was before its last move | `n.<id>.home` | the parent it moved from: the one place a layer is put back when a cycle must be broken |
//! | the file's own properties | `d.<prop>` | name, current page, document colours, style/variable orders ... ([`DocProp`]) |
//! | styles, variables, collections, comment threads | `e.<kind>:<id>` | one key per entry ([`Entry`]), the whole entry as JSON |
//!
//! A layer is created by `ensure n.<id>` and its keys; deleting a layer removes its key and every key of what was inside it. What a
//! window shows is always a pure function of the shared tree ([`Shim`]'s reconcile): a layer is shown when it chains up to a page
//! through `parent` keys that exist. So a layer made in a frame another person deleted at the same time is gone in every window.
//!
//! ★ THE CYCLE RULE: two people moving two layers into each other at once (A into B while B moves into A) make a cycle of `parent` keys,
//! and nothing in it chains up to a page. The cycle is broken the same way in every window: the layer with the LOWEST id on the cycle
//! goes back to the parent it came from (its `home`, or when that is gone or itself inside the cycle, the first page), and the others
//! stay where they were moved. Nothing vanishes, and every window agrees because the rule reads only the shared tree.
//!
//! Not shared live: pictures larger than one shared value (about 60 KB; smaller ones travel with the document), the library a file
//! publishes, plugin settings, view state (zoom, selection). A layer property larger than one shared value is refused loudly
//! ([`ShimError::TooLarge`]) and the frame falls back to single-editor.
//!
//! The frame's own edits are not echoed back by the host, so the shim keeps its view of the shared tree itself: the host's truth
//! (`confirmed`) plus the local batches the host has not yet acknowledged (`pending`), re-applied over every delta (the contract's
//! client-side prediction). A batch counts as acknowledged when a message carries `ack >= seq`, or after [`PENDING_MS`].
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod mirror;

use std::collections::{BTreeMap, BTreeSet};

use mirror::{Mirror, spans_to_chars};

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

fn path_of(op: &Value) -> Vec<String> {
    op.get("path").and_then(Value::as_array).map(|p| p.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

/// Apply one op of the contract to a tree (the mirror's vocabulary: maps, texts and lists).
fn apply_op(root: &mut Value, op: &Value) -> Result<(), ShimError> {
    let mut m = Mirror::new(std::mem::take(root));
    let r = m.apply(op).map_err(|e| ShimError::Tree(e.0));
    *root = m.root;
    r
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

/// Properties that are objects: a marker key holds `null` or `{}` and every FIELD is a key of its own (`layout.gap`), so two people
/// changing different fields both land.
const SPLIT: [Prop; 2] = [Prop::Layout, Prop::ChildLayout];

fn field_key(p: Prop, field: &str) -> String {
    format!("{}.{field}", prop_key(p))
}

/// The property a layer key belongs to (`layout`, `layout.gap` -> Layout); other keys (`tx`, `home`) belong to none.
fn prop_of_key(key: &str) -> Option<Prop> {
    match key.split_once('.') {
        Some((base, _)) => parse_prop(base).filter(|p| SPLIT.contains(p)),
        None => parse_prop(key),
    }
}

/// A text layer's characters as the tree holds them.
fn tx_chars(m: &Map<String, Value>) -> Option<String> {
    m.get("tx")?.get("$text").map(|spans| spans_to_chars(spans).into_iter().map(|(c, _)| c).collect())
}

/// The `kind` of a layer with a text layer's characters put back in.
fn read_kind(m: &Map<String, Value>) -> Option<Json> {
    let mut j = json_of(m.get("kind")?.as_str()?)?;
    if j.0.get("type").and_then(Value::as_str) == Some("text")
        && let Some(chars) = tx_chars(m)
        && let Some(o) = j.0.as_object_mut()
    {
        o.insert("characters".into(), Value::String(chars));
    }
    Some(j)
}

/// A property of a layer as the tree holds it.
fn read_prop(m: &Map<String, Value>, p: Prop) -> Option<Json> {
    if p == Prop::Kind {
        return read_kind(m);
    }
    if SPLIT.contains(&p) {
        let marker = json_of(m.get(&prop_key(p))?.as_str()?)?;
        let Value::Object(mut obj) = marker.0 else { return Some(marker) };
        let prefix = format!("{}.", prop_key(p));
        for (k, v) in m {
            if let Some(field) = k.strip_prefix(&prefix)
                && let Some(j) = v.as_str().and_then(json_of)
            {
                obj.insert(field.to_owned(), j.0);
            }
        }
        return Some(Json(Value::Object(obj)));
    }
    m.get(&prop_key(p))?.as_str().and_then(json_of)
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

/// The parent a layer was moved from (what it goes back to when it must leave a cycle).
fn home_of(m: &Map<String, Value>) -> Option<NodeId> {
    json_of(m.get("home")?.as_str()?)?.0.as_u64().map(NodeId)
}

fn stored_parent(tree: &Value, id: NodeId) -> Option<Option<NodeId>> {
    layer(tree, id).and_then(parent_of)
}

/// The layers of the cycle the `parent` keys run into from `id` (empty: they reach a page, or end).
fn find_cycle(tree: &Value, id: NodeId) -> Vec<NodeId> {
    let mut path: Vec<NodeId> = Vec::new();
    let mut cur = id;
    for _ in 0..MAX_DEPTH {
        if let Some(at) = path.iter().position(|x| *x == cur) {
            return path[at..].to_vec();
        }
        path.push(cur);
        match stored_parent(tree, cur) {
            Some(Some(p)) => cur = p,
            _ => return Vec::new(),
        }
    }
    Vec::new()
}

/// Whether `id` is the layer the cycle rule moves: on a cycle, and the lowest id of it.
fn is_breaker(tree: &Value, id: NodeId) -> bool {
    let cycle = find_cycle(tree, id);
    cycle.contains(&id) && cycle.iter().min() == Some(&id)
}

/// Whether `h` chains up to a page without passing through `cycle`.
fn grounded(tree: &Value, h: NodeId, cycle: &[NodeId]) -> bool {
    let mut cur = h;
    for _ in 0..MAX_DEPTH {
        if cycle.contains(&cur) {
            return false;
        }
        match stored_parent(tree, cur) {
            Some(None) => return true,
            Some(Some(p)) => cur = p,
            None => return false,
        }
    }
    false
}

/// The page with the lowest id.
fn lowest_page(tree: &Value) -> Option<NodeId> {
    tree.get("n")?.as_object()?.iter().filter_map(|(k, v)| parent_of(v.as_object()?)?.is_none().then(|| parse_node(k)).flatten()).min()
}

/// Where the cycle rule puts the breaker `id`: its `home`, else the first page.
fn rescue(tree: &Value, id: NodeId) -> Option<NodeId> {
    let cycle = find_cycle(tree, id);
    match layer(tree, id).and_then(home_of) {
        Some(h) if grounded(tree, h, &cycle) => Some(h),
        _ => lowest_page(tree),
    }
}

/// The parent a window shows for `id`: the stored one, except for the layer the cycle rule moves.
/// `Some(None)`: a page; `None`: no usable parent.
fn effective_parent(tree: &Value, id: NodeId) -> Option<Option<NodeId>> {
    let stored = stored_parent(tree, id)?;
    if stored.is_some() && is_breaker(tree, id) {
        return rescue(tree, id).map(Some);
    }
    Some(stored)
}

/// Whether `id` chains up to a page through layers the tree holds: the one rule that decides what a window shows.
fn reachable(tree: &Value, id: NodeId) -> bool {
    let mut cur = id;
    for _ in 0..MAX_DEPTH {
        match effective_parent(tree, cur) {
            Some(None) => return true,
            Some(Some(p)) => cur = p,
            None => return false,
        }
    }
    false
}

fn depth_of(tree: &Value, id: NodeId) -> usize {
    let (mut cur, mut d) = (id, 0);
    while let Some(Some(p)) = effective_parent(tree, cur) {
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
    /// The layers the cycle rule moved at the last look (looked at again, in case the cycle is gone).
    broken: BTreeSet<NodeId>,
    reshown: bool,
    /// Changes the replica could not apply (it skips them identically in every window). Stays 0 in a healthy room.
    pub skipped: usize,
}

impl Shim {
    /// A shim whose window makes layer ids in the space of `client` (not 0; two windows of one room must differ).
    #[must_use]
    pub fn new(client: u32) -> Self {
        Shim { client, replica: None, view: json!({}), confirmed: None, pending: Vec::new(), seq: 0, broken: BTreeSet::new(), reshown: false, skipped: 0 }
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
        let batches = self.emit(ops, label, join, false, now_ms)?;
        // A layer the cycle rule moved is looked at again: this edit may have undone the cycle, and then it goes back to where its
        // own move put it (the window's design is then not the editor's: see [`Shim::take_reshown`]).
        if !self.broken.is_empty() {
            let mut touch = Touch::none();
            for op in batches.iter().flat_map(|b| &b.ops) {
                let mut p = path_of(op);
                if let Some(k) = op.get("key").and_then(Value::as_str) {
                    p.push(k.to_owned());
                }
                touch.note(&p);
            }
            if self.show(&touch)? {
                self.reshown = true;
            }
        }
        Ok(batches)
    }

    /// Whether a local edit made the shim change the design it holds (the cycle rule): the caller shows [`Shim::document`] again.
    pub fn take_reshown(&mut self) -> bool {
        std::mem::take(&mut self.reshown)
    }

    /// The ops one change makes. `old` is the document before it (a deleted layer's subtree is what it held).
    fn ops_for(&self, change: &Change, old: &Document, ensured: &mut BTreeSet<NodeId>, out: &mut Vec<Value>) -> Result<(), ShimError> {
        let held = |path: &[&str], key: &str, text: &str| -> bool { self.holds(path, key, text) };
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
                self.kind_ops(&k, &kind.0, out)?;
                out.push(op_set(&path, "parent", &text_of(&Json::of(parent)?)));
                out.push(op_set(&path, "home", &text_of(&Json::of(parent)?)));
                out.push(op_set(&path, "position", &text_of(&Json::of(position)?)));
                for (p, v) in props {
                    match p {
                        Prop::Kind => self.kind_ops(&k, &v.0, out)?,
                        p if SPLIT.contains(p) => self.split_ops(&k, *p, &v.0, out)?,
                        _ => out.push(op_set(&path, &prop_key(*p), &checked(&format!("layer {id} {}", prop_key(*p)), text_of(v))?)),
                    }
                }
            }
            Change::Set { id, prop, value } => {
                let k = node_key(*id);
                let path = ["n", k.as_str()];
                let mut body = Vec::new();
                match prop {
                    Prop::Kind => self.kind_ops(&k, &value.0, &mut body)?,
                    p if SPLIT.contains(p) => self.split_ops(&k, *p, &value.0, &mut body)?,
                    _ => {
                        let text = checked(&format!("layer {id} {}", prop_key(*prop)), text_of(value))?;
                        if !held(&path, &prop_key(*prop), &text) {
                            body.push(op_set(&path, &prop_key(*prop), &text));
                        }
                    }
                }
                // A layer remembers where it was before it moved: the one place a cycle's lowest layer is put back.
                if *prop == Prop::Parent {
                    let was = old.node(*id).and_then(|n| n.parent);
                    let text = text_of(&Json::of(&was)?);
                    if !held(&path, "home", &text) {
                        body.push(op_set(&path, "home", &text));
                    }
                }
                if !body.is_empty() {
                    // A layer another person deleted at the same time may be gone from this window's copy of the room: the
                    // write makes its container again (without a parent, so the layer stays deleted), never a refused batch.
                    if ensured.insert(*id) {
                        out.push(op_ensure(&["n"], &k));
                    }
                    out.extend(body);
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

    /// Whether the tree already holds `text` at `path`/`key`.
    fn holds(&self, path: &[&str], key: &str, text: &str) -> bool {
        let mut at = &self.view;
        for seg in path {
            match at.get(*seg) {
                Some(v) => at = v,
                None => return false,
            }
        }
        at.get(key).and_then(Value::as_str) == Some(text)
    }

    fn layer_map(&self, k: &str) -> Option<&Map<String, Value>> {
        self.view.get("n")?.get(k)?.as_object()
    }

    /// A layer's type: a text layer's characters are the CRDT text `tx` (edited by the characters that changed), everything else of
    /// the type is the `kind` key (a text layer's with empty `characters`).
    fn kind_ops(&self, k: &str, kind: &Value, out: &mut Vec<Value>) -> Result<(), ShimError> {
        let path = ["n", k];
        let held_tx = self.layer_map(k).and_then(tx_chars);
        let mut stripped = kind.clone();
        if kind.get("type").and_then(Value::as_str) == Some("text") {
            let chars = kind.get("characters").and_then(Value::as_str).unwrap_or_default();
            if chars.len() > MAX_VALUE {
                return Err(ShimError::TooLarge(format!("the characters of text layer {k} are {} bytes, and the room holds at most {MAX_VALUE} in one value", chars.len())));
            }
            if let Some(o) = stripped.as_object_mut() {
                o.insert("characters".into(), Value::String(String::new()));
            }
            match held_tx {
                None => out.push(json!({"op": "new", "path": path, "key": "tx", "kind": "text", "text": chars})),
                Some(old) => text_diff_ops(&["n", k, "tx"], &old, chars, out),
            }
        } else if held_tx.is_some() {
            out.push(op_remove(&path, "tx"));
        }
        let text = text_of(&Json(stripped));
        if text.len() > MAX_VALUE {
            return Err(ShimError::TooLarge(format!("a layer's type is {} bytes, and the room holds at most {MAX_VALUE} in one value", text.len())));
        }
        if !self.holds(&path, "kind", &text) {
            out.push(op_set(&path, "kind", &text));
        }
        Ok(())
    }

    /// An object property (auto layout): the marker key, then one key per field that differs; fields that went are removed.
    fn split_ops(&self, k: &str, prop: Prop, value: &Value, out: &mut Vec<Value>) -> Result<(), ShimError> {
        let path = ["n", k];
        let pk = prop_key(prop);
        let mut wanted: BTreeSet<String> = BTreeSet::new();
        match value {
            Value::Object(fields) => {
                if !self.holds(&path, &pk, "{}") {
                    out.push(op_set(&path, &pk, "{}"));
                }
                for (f, v) in fields {
                    let key = field_key(prop, f);
                    let text = v.to_string();
                    if text.len() > MAX_VALUE {
                        return Err(ShimError::TooLarge(format!("layer {k} {key} is {} bytes, and the room holds at most {MAX_VALUE} in one value", text.len())));
                    }
                    if !self.holds(&path, &key, &text) {
                        out.push(op_set(&path, &key, &text));
                    }
                    wanted.insert(key);
                }
            }
            other => {
                let text = other.to_string();
                if !self.holds(&path, &pk, &text) {
                    out.push(op_set(&path, &pk, &text));
                }
            }
        }
        let prefix = format!("{pk}.");
        if let Some(m) = self.layer_map(k) {
            for key in m.keys().filter(|key| key.starts_with(&prefix) && !wanted.contains(*key)) {
                out.push(op_remove(&path, key));
            }
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
        let (changes, broken) = reconcile(&self.view, replica, touch, &self.broken)?;
        self.broken = broken;
        if changes.is_empty() {
            return Ok(false);
        }
        let mut next = replica.clone();
        let applied = next.apply(&changes, Mode::Lenient)?;
        if applied.skipped == 0 {
            self.replica = Some(next);
            return Ok(true);
        }
        // Some change was refused where it stood - typically a move into a layer that is still inside the layer being moved, whose own
        // move comes later in the list. One at a time, what is refused waits for the others and is tried again.
        let mut next = replica.clone();
        let mut queue = changes;
        loop {
            let before = queue.len();
            let mut refused = Vec::new();
            for c in queue {
                if next.apply(std::slice::from_ref(&c), Mode::Lenient)?.skipped > 0 {
                    refused.push(c);
                }
            }
            queue = refused;
            if queue.is_empty() || queue.len() == before {
                break;
            }
        }
        self.skipped += queue.len();
        self.replica = Some(next);
        Ok(true)
    }
}

/// The changes that take `replica` to the layers and entries `tree` holds, for what `touch` names.
fn reconcile(tree: &Value, replica: &Replica, touch: &Touch, prev_broken: &BTreeSet<NodeId>) -> Result<(Vec<Change>, BTreeSet<NodeId>), ShimError> {
    let doc = replica.document();
    let mut ids: BTreeSet<NodeId> = match &touch.nodes {
        Some(s) => s.clone(),
        None => {
            let mut all: BTreeSet<NodeId> = tree.get("n").and_then(Value::as_object).map(|m| m.keys().filter_map(|k| parse_node(k)).collect()).unwrap_or_default();
            all.extend(doc.nodes().map(|n| n.id));
            all
        }
    };
    // The cycle rule: a layer on a cycle brings the whole cycle, and the layers an earlier look moved are looked at again.
    let mut more: BTreeSet<NodeId> = prev_broken.clone();
    for id in &ids {
        more.extend(find_cycle(tree, *id));
    }
    ids.extend(more);
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
    let broken: BTreeSet<NodeId> = ids.iter().copied().filter(|id| is_breaker(tree, *id)).collect();
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
        let mut props: Vec<Prop> = m.keys().filter_map(|k| prop_of_key(k)).collect::<BTreeSet<_>>().into_iter().collect();
        props.sort_by_key(|p| match p {
            Prop::Parent => 0,
            Prop::Position => 1,
            _ => 2,
        });
        for p in props {
            // The parent a window shows is the stored one, except for the layer the cycle rule moves.
            let want = if p == Prop::Parent { effective_parent(tree, id).and_then(|e| Json::of(&e).ok()) } else { read_prop(m, p) };
            let Some(want) = want else { continue };
            let have = replica.value(&Target::Node(id, p))?;
            if have.as_ref() != Some(&want) {
                sets.push(Change::Set { id, prop: p, value: want });
            }
        }
        // A property the tree holds no key for is what a new layer of that type holds: a key that was removed (an undo of the first
        // write to it) puts the layer's property back to that.
        let written: BTreeSet<Prop> = m.keys().filter_map(|k| prop_of_key(k)).collect();
        if let Some(kind) = read_kind(m) {
            for &p in Prop::FIELDS.iter().filter(|p| **p != Prop::Kind && !written.contains(*p)) {
                let Ok(want) = p.default_for(&kind) else { continue };
                if replica.value(&Target::Node(id, p))?.as_ref() != Some(&want) {
                    sets.push(Change::Set { id, prop: p, value: want });
                }
            }
        }
    }
    creates.sort();
    let mut out: Vec<Change> = Vec::new();
    for (_, id) in creates {
        let Some(m) = layer(tree, id) else { continue };
        let read = |k: &str| m.get(k).and_then(Value::as_str).and_then(json_of);
        let (Some(kind), Some(position)) = (read_kind(m), read("position")) else { continue };
        let Some(parent) = effective_parent(tree, id) else { continue };
        let Ok(position) = position.read::<String>("position") else { continue };
        let mut props = Vec::new();
        let named: BTreeSet<Prop> = m.keys().filter_map(|k| prop_of_key(k)).collect();
        for p in named {
            if !matches!(p, Prop::Kind | Prop::Parent | Prop::Position)
                && let Some(j) = read_prop(m, p)
            {
                props.push((p, j));
            }
        }
        out.push(Change::Create { id, kind, parent, position, props });
    }
    // The layer the cycle rule moves goes first, so the layer that moved into it never makes a cycle in the replica.
    sets.sort_by_key(|c| !matches!(c, Change::Set { id, prop: Prop::Parent, .. } if broken.contains(id)));
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
    Ok((out, broken))
}

/// The text ops that take the characters `old` to `new`: what changed in the middle (the common start and end are kept), so two people
/// typing in different places of one text merge.
fn text_diff_ops(path: &[&str], old: &str, new: &str, out: &mut Vec<Value>) {
    let (o, n): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    let prefix = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
    let suffix = o[prefix..].iter().rev().zip(n[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    let deleted = o.len() - prefix - suffix;
    let inserted: String = n[prefix..n.len() - suffix].iter().collect();
    if deleted > 0 {
        out.push(json!({"op": "text.delete", "path": path, "at": prefix, "len": deleted}));
    }
    if !inserted.is_empty() {
        out.push(json!({"op": "text.insert", "path": path, "at": prefix, "text": inserted}));
    }
}

/// The most layers a cursor body names.
pub const MAX_CURSOR_SELECTION: usize = 20;

/// Where another person is, resolved against this window's design.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerCursor {
    /// The page they are on (one this design has).
    pub page: NodeId,
    /// Their pointer, in page coordinates.
    pub at: Option<(f64, f64)>,
    /// The layers they have selected that this design has.
    pub selection: Vec<NodeId>,
}

/// The body of a `craft:crdt:cursor` message: the page, the pointer in page coordinates and the selected layers (by id: the same layer
/// in every window).
#[must_use]
pub fn cursor_for(page: NodeId, at: Option<(f64, f64)>, selection: &[NodeId]) -> Value {
    let mut body = json!({"pg": page.0});
    if let Some((x, y)) = at.filter(|(x, y)| x.is_finite() && y.is_finite()) {
        body["x"] = json!(x);
        body["y"] = json!(y);
    }
    if !selection.is_empty() {
        body["sel"] = json!(selection.iter().take(MAX_CURSOR_SELECTION).map(|n| n.0).collect::<Vec<_>>());
    }
    body
}

/// Another person's cursor body, resolved against `doc`; `None` for a page this design does not have.
#[must_use]
pub fn peer_cursor(doc: &Document, body: &Value) -> Option<PeerCursor> {
    let page = NodeId(body.get("pg")?.as_u64()?);
    if !doc.pages.contains(&page) {
        return None;
    }
    let at = match (body.get("x").and_then(Value::as_f64), body.get("y").and_then(Value::as_f64)) {
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Some((x, y)),
        _ => None,
    };
    let selection = body.get("sel").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(NodeId).filter(|n| doc.contains(*n)).take(MAX_CURSOR_SELECTION).collect()).unwrap_or_default();
    Some(PeerCursor { page, at, selection })
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
