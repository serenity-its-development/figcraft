//! OneOS: the token-constrained mode of a compose-bound document (Mona 2026-10-27: FigCraft IS the Pages & apps editor).
//!
//! compose/v1 is a closed, token-only grammar: gaps and paddings are `sm|md|lg`, colours are the theme's roles, type is the
//! theme's scale. A page frame the host bridge draws carries `oneos.page` in its plugin data; once the host has stated the
//! theme's tokens ([`Session::set_compose_tokens`]) every edit that leaves a raw spacing, colour, font size or radius on a
//! layer beneath such a frame is REFUSED BY NAME and rolled back (the same funnel as the other `check_*` guards in
//! [`Session::execute`]), so the canvas never holds a value compose would silently drop. Layers outside a page frame are
//! unconstrained (the reverse bridge refuses what compose cannot hold).
//!
//! The `oneos.*` plugin data (the sidecar that carries each node's whole definition) is written by the host's bridge alone: a
//! run that is not the host's (`trusted`) may not set it with `node.set`; the typed `bind.field` command is how a field is bound.

use std::sync::Arc;

use figcraft_doc::{Document, Node, NodeId, NodeKind};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

/// The plugin-data key that marks a page frame of a compose document.
pub const PAGE_KEY: &str = "oneos.page";
/// The sidecar namespace the host's bridge owns.
pub const SIDECAR_PREFIX: &str = "oneos.";
/// Where `bind.field` records which field feeds which property.
pub const BIND_KEY: &str = "oneos.bindField";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tokens {
    pub spacing: Vec<(String, f64)>,
    pub radii: Vec<(String, f64)>,
    pub type_sizes: Vec<(String, f64)>,
    /// Colour role -> `#RRGGBB` (upper case).
    pub colors: Vec<(String, String)>,
}

fn table(v: Option<&Value>, what: &str) -> std::result::Result<Vec<(String, f64)>, String> {
    let Some(o) = v.and_then(Value::as_object) else { return Err(format!("tokens.{what}: a table of name -> number")) };
    let mut out = Vec::new();
    for (k, x) in o {
        let n = x.as_f64().filter(|n| n.is_finite() && *n >= 0.0).ok_or_else(|| format!("tokens.{what}.{k}: a number"))?;
        out.push((k.clone(), n));
    }
    out.sort_by(|a, b| a.1.total_cmp(&b.1));
    Ok(out)
}

fn hex6(s: &str) -> Option<String> {
    let t = s.trim().trim_start_matches('#');
    let t = match t.len() {
        6 => t,
        8 if t[6..].eq_ignore_ascii_case("ff") => &t[..6],
        _ => return None,
    };
    t.bytes().all(|b| b.is_ascii_hexdigit()).then(|| format!("#{}", t.to_ascii_uppercase()))
}

impl Tokens {
    pub fn from_json(v: &Value) -> std::result::Result<Self, String> {
        let colors = v.get("colors").and_then(Value::as_object).ok_or("tokens.colors: a table of role -> #RRGGBB")?;
        let mut cs = Vec::new();
        for (role, c) in colors {
            let h = c.as_str().and_then(hex6).ok_or_else(|| format!("tokens.colors.{role}: a #RRGGBB colour"))?;
            cs.push((role.clone(), h));
        }
        Ok(Self {
            spacing: table(v.get("spacing"), "spacing")?,
            radii: table(v.get("radii"), "radii")?,
            type_sizes: table(v.get("type"), "type")?,
            colors: cs,
        })
    }
}

fn near(t: &[(String, f64)], v: f64) -> String {
    let list = t.iter().map(|(k, x)| format!("{k} {x}")).collect::<Vec<_>>().join(", ");
    let best = t.iter().min_by(|a, b| (a.1 - v).abs().total_cmp(&(b.1 - v).abs()));
    match best {
        Some((k, x)) => format!("nearest is {k} ({x}); the scale is {list}"),
        None => "the scale is empty".into(),
    }
}

fn on_scale(t: &[(String, f64)], v: f64) -> bool {
    t.iter().any(|(_, x)| (x - v).abs() < 1e-6)
}

impl Tokens {
    /// Every raw value on `n` (one layer), named. Empty when the layer holds only tokens.
    pub fn violations(&self, n: &Node) -> Vec<String> {
        let mut out = Vec::new();
        let who = format!("\"{}\"", n.name);
        if let Some(l) = &n.layout {
            let bound = |f: &str| n.bound.contains_key(f);
            if !bound("gap") && !on_scale(&self.spacing, l.gap) {
                out.push(format!("{who}: gap {} is not a spacing token ({})", l.gap, near(&self.spacing, l.gap)));
            }
            if l.wrap && !bound("counterAxisSpacing") && !on_scale(&self.spacing, l.cross_gap) {
                out.push(format!("{who}: row gap {} is not a spacing token ({})", l.cross_gap, near(&self.spacing, l.cross_gap)));
            }
            for (side, p) in ["top", "right", "bottom", "left"].iter().zip(l.padding) {
                if !on_scale(&self.spacing, p) {
                    out.push(format!("{who}: {side} padding {p} is not a spacing token ({})", near(&self.spacing, p)));
                }
            }
        }
        for r in n.corners.radii {
            let r = f64::from(r);
            if r > 0.0 && !on_scale(&self.radii, r) {
                out.push(format!("{who}: corner radius {r} is not a radius token ({})", near(&self.radii, r)));
                break;
            }
        }
        if let NodeKind::Text(t) = &n.kind {
            let s = f64::from(t.style.font_size);
            if !n.bound.contains_key("fontSize") && !on_scale(&self.type_sizes, s) {
                out.push(format!("{who}: font size {s} is not a type token ({})", near(&self.type_sizes, s)));
            }
        }
        for (what, paints) in [("fill", &n.fills), ("stroke", &n.strokes)] {
            for (i, p) in paints.iter().enumerate() {
                if !p.visible || n.bound.contains_key(&format!("{what}s/{i}/color")) {
                    continue;
                }
                match &p.kind {
                    figcraft_color::PaintKind::Solid { color } => {
                        let h = hex6(&color.to_css_hex()).unwrap_or_default();
                        if !self.colors.iter().any(|(_, c)| *c == h) {
                            let roles = self.colors.iter().map(|(k, c)| format!("{k} {c}")).collect::<Vec<_>>().join(", ");
                            out.push(format!("{who}: {what} colour {h} is not a theme colour role ({roles})"));
                        }
                    }
                    figcraft_color::PaintKind::Image { .. } => {}
                    _ => out.push(format!("{who}: a {what} that is not a solid theme colour (gradients are not a theme token)")),
                }
            }
        }
        if !n.effects.is_empty() {
            out.push(format!("{who}: effects (shadows, blurs) are not theme tokens"));
        }
        out
    }
}

/// The compose page frame this layer is, or sits beneath.
fn page_frame(doc: &Document, mut id: NodeId) -> Option<NodeId> {
    for _ in 0..64 {
        let n = doc.node(id)?;
        if n.plugin_data.contains_key(PAGE_KEY) {
            return Some(id);
        }
        id = n.parent?;
    }
    None
}

/// Is this layer, or one above it, a compose page frame?
fn on_page(doc: &Document, id: NodeId) -> bool {
    page_frame(doc, id).is_some()
}

/// The blocks the canvas draws and edits itself; `section.insert` places only the others (locked OneOS blocks).
const CANVAS_BLOCKS: &[&str] = &["section", "stack", "grid", "columns", "split", "card", "hero", "heading", "badge", "button", "divider", "spacer", "text"];
/// What a block's node may state (`kids` are layers inside the placeholder, drawn after the insert).
const NODE_KEYS: &[&str] = &["c", "p", "bind", "show", "on", "bp"];
const MAX_NODE_BYTES: usize = 64 * 1024;

fn token_px(t: &[(String, f64)], name: &str, fallback: f64) -> f64 {
    t.iter().find(|(k, _)| k == name).or_else(|| t.iter().find(|(_, v)| *v > 0.0)).map_or(fallback, |(_, v)| *v)
}

fn role(t: &[(String, String)], name: &str, fallback: &str) -> String {
    t.iter().find(|(k, _)| k == name).or_else(|| t.first()).map_or_else(|| fallback.to_string(), |(_, v)| v.clone())
}

impl Session {
    /// State the theme's tokens (or `None` / null to lift the constraint). Spacing, radii, type and colours are tables of name -> value.
    pub fn set_compose_tokens(&mut self, v: Option<&Value>) -> std::result::Result<Value, String> {
        self.compose_tokens = match v {
            Some(v) if !v.is_null() => Some(Tokens::from_json(v)?),
            _ => None,
        };
        Ok(json!({ "constrained": self.compose_tokens.is_some() }))
    }

    /// After a command: every layer it changed beneath a page frame holds tokens only, and only the host writes the sidecar.
    pub(crate) fn check_compose_tokens(&self, id: &str, before: &Option<(Arc<Document>, Vec<NodeId>, NodeId, u64)>) -> Result<()> {
        let Some(tokens) = &self.compose_tokens else { return Ok(()) };
        let judge_tokens = !self.defer_token_check || id == "command.batch";
        // The sidecar is judged at every step (the end state of a batch cannot say who wrote it).
        let judge_sidecar = !self.host_trusted && id != "command.batch";
        let Some((bdoc, _, _, uid)) = before else { return Ok(()) };
        let Some(ds) = self.docs.iter().find(|d| d.uid == *uid) else { return Ok(()) };
        if Arc::ptr_eq(&ds.doc, bdoc) {
            return Ok(());
        }
        let mut problems = Vec::new();
        for n in ds.doc.nodes() {
            let same = bdoc.node_arc(n.id).zip(ds.doc.node_arc(n.id)).is_some_and(|(a, b)| Arc::ptr_eq(a, b));
            if same || !on_page(&ds.doc, n.id) {
                continue;
            }
            if judge_sidecar {
                let old = bdoc.node(n.id);
                let sidecar = |k: &String| k.starts_with(SIDECAR_PREFIX);
                let changed = n.plugin_data.iter().any(|(k, v)| sidecar(k) && old.and_then(|o| o.plugin_data.get(k)) != Some(v))
                    || old.is_some_and(|o| o.plugin_data.keys().any(|k| sidecar(k) && !n.plugin_data.contains_key(k)));
                if changed {
                    problems.push(format!(
                        "\"{}\": the oneos.* sidecar is written by the Pages & apps panel and bind.field, not by a free property edit",
                        n.name
                    ));
                }
            }
            if judge_tokens {
                problems.extend(tokens.violations(n));
            }
        }
        if problems.is_empty() {
            return Ok(());
        }
        problems.truncate(6);
        Err(EngineError::Other(format!("token-refused: {}", problems.join("; "))))
    }

    /// The scale a numeric field of `ids` is held to, or `None` when the layers are not (all) on a compose page or no tokens are stated:
    /// the editor OFFERS these (name, px) instead of a free number, so a person never meets a refusal by doing the natural thing.
    /// Spacing for gaps and padding, radii for corners, the type scale for a font size; zero (`none`) is always on the spacing and radius scales.
    pub fn compose_scale(&self, ids: &[NodeId], field: &str) -> Option<Vec<(String, f64)>> {
        let t = self.compose_tokens.as_ref()?;
        if !self.all_on_page(ids) {
            return None;
        }
        let with_none = |v: &[(String, f64)]| {
            let mut out = v.to_vec();
            if !out.iter().any(|(_, x)| *x == 0.0) {
                out.insert(0, ("none".into(), 0.0));
            }
            out
        };
        match field {
            "gap" | "rowGap" | "crossGap" | "counterAxisSpacing" | "padding" | "paddingTop" | "paddingRight" | "paddingBottom" | "paddingLeft"
            | "paddingHorizontal" | "paddingVertical" => Some(with_none(&t.spacing)),
            "cornerRadius" | "topLeftRadius" | "topRightRadius" | "bottomLeftRadius" | "bottomRightRadius" => Some(with_none(&t.radii)),
            "fontSize" => Some(t.type_sizes.clone()),
            _ => None,
        }
    }

    /// The theme's colour roles (role, `#RRGGBB`) a fill, stroke or text colour of `ids` is chosen from, or `None` when the layers are not
    /// (all) on a compose page or no tokens are stated.
    pub fn compose_colors(&self, ids: &[NodeId]) -> Option<Vec<(String, String)>> {
        let t = self.compose_tokens.as_ref()?;
        self.all_on_page(ids).then(|| t.colors.clone())
    }

    /// A dragged value snapped to the nearest token of the field's scale (a handle never lands off the scale); unchanged off a page.
    pub fn compose_snap(&self, ids: &[NodeId], field: &str, v: f64) -> f64 {
        match self.compose_scale(ids, field) {
            Some(scale) => scale.iter().map(|(_, x)| *x).min_by(|a, b| (a - v).abs().total_cmp(&(b - v).abs())).unwrap_or(v),
            None => v,
        }
    }

    fn all_on_page(&self, ids: &[NodeId]) -> bool {
        let Ok(doc) = self.doc() else { return false };
        !ids.is_empty() && ids.iter().all(|i| on_page(doc, *i))
    }

    /// The `oneos.id` of the one selected layer, or "" (none selected, several, or a layer the bridge did not draw).
    pub fn selected_oneos_id(&self) -> String {
        let Ok(ds) = self.ds() else { return String::new() };
        match ds.selection.as_slice() {
            [one] => ds.doc.node(*one).and_then(|n| n.plugin_data.get("oneos.id")).cloned().unwrap_or_default(),
            _ => String::new(),
        }
    }

    /// `section.insert {node, parent, index?, name?}`: place a locked OneOS block (a form, a live list or table, a metric, a menu ...) on the
    /// Expanded frame of a compose page as a LOCKED component instance. `node` is the compose node as JSON text, `{c, p, bind?, show?, on?}`:
    /// the block's properties live in the OneOS panel, never on the canvas, and the compose validator judges them (an AI run is tried
    /// first and refused with the validator's message). The sidecar is written here, by the host's typed command, never by `node.set`.
    pub(crate) fn section_insert(&mut self, args: &Value) -> std::result::Result<Value, String> {
        let node: Value = match args.get("node") {
            Some(Value::String(s)) if s.len() <= MAX_NODE_BYTES => serde_json::from_str(s).map_err(|e| format!("section.insert: `node` is the block as JSON text ({e})"))?,
            Some(Value::String(_)) => return Err(format!("section.insert: `node` is at most {MAX_NODE_BYTES} bytes")),
            Some(v @ Value::Object(_)) if v.to_string().len() <= MAX_NODE_BYTES => v.clone(),
            _ => return Err("section.insert: `node` is the compose block {c, p, bind?, show?, on?} as JSON text".into()),
        };
        let obj = node.as_object().ok_or("section.insert: `node` is a JSON object {c, p, ...}")?;
        let c = obj.get("c").and_then(Value::as_str).unwrap_or("");
        if c.is_empty() || c.len() > 32 || !c.starts_with(|b: char| b.is_ascii_lowercase()) || !c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(format!("section.insert: `node.c` is a compose block name like form, list, table or metric (got '{c}')"));
        }
        if CANVAS_BLOCKS.contains(&c) {
            return Err(format!("section.insert: a {c} is drawn and edited on the canvas (frame.create, text.create, node.set), not inserted; section.insert places a locked OneOS block"));
        }
        if let Some(k) = obj.keys().find(|k| !NODE_KEYS.contains(&k.as_str())) {
            return Err(format!("section.insert: a block states c, p, bind, show, on and bp, not '{k}' (children are layers inside the placeholder)"));
        }
        if obj.get("p").is_some_and(|p| !p.is_object()) {
            return Err("section.insert: `node.p` is an object of the block's properties".into());
        }
        let parent = args.get("parent").and_then(Value::as_u64).ok_or("section.insert: `parent` is the layer to place the block in")?;
        let (oneos_id, geometry_w) = {
            let doc = self.doc().map_err(|e| e.to_string())?;
            if !doc.contains(NodeId(parent)) {
                return Err(format!("no layer {parent}"));
            }
            let Some(frame) = page_frame(doc, NodeId(parent)) else {
                return Err(format!("section.insert: layer {parent} is not on a compose page; place the block inside a page's layers"));
            };
            let class = doc.node(frame).and_then(|f| f.plugin_data.get("oneos.cls")).map_or("expanded", String::as_str);
            if class != "expanded" {
                return Err(format!("section.insert: blocks are placed on the Expanded frame (this is the {class} frame); the other sizes follow it"));
            }
            let taken: std::collections::HashSet<&str> =
                doc.nodes().filter_map(|n| n.plugin_data.get("oneos.id")).map(String::as_str).collect();
            let stem: String = c.to_ascii_lowercase().chars().take(26).collect();
            let id = (1..10_000).map(|i| format!("{stem}-{i}")).find(|i| !taken.contains(i.as_str())).ok_or("section.insert: no free block id")?;
            (id, 500.0)
        };
        let (pad, gap, size, fill, edge, ink) = match &self.compose_tokens {
            Some(t) => (
                token_px(&t.spacing, "md", 16.0),
                token_px(&t.spacing, "none", 0.0),
                token_px(&t.type_sizes, "caption", 13.0),
                role(&t.colors, "placeholder", "#ECECEC"),
                role(&t.colors, "placeholderEdge", "#9E9E9E"),
                role(&t.colors, "placeholderText", "#666666"),
            ),
            None => (16.0, 0.0, 13.0, "#ECECEC".into(), "#9E9E9E".into(), "#666666".into()),
        };
        let title = obj.get("p").and_then(|p| p.get("title").or_else(|| p.get("label"))).and_then(Value::as_str).filter(|t| !t.is_empty() && t.len() <= 60);
        let shown = title.map_or_else(|| c.to_string(), |t| format!("{c}: {t}"));
        let name = args.get("name").and_then(Value::as_str).filter(|n| !n.is_empty() && n.len() <= 80).unwrap_or(&oneos_id).to_string();
        let was = std::mem::replace(&mut self.host_trusted, true);
        let made = self.execute(
            "node.create",
            &json!({"type": "frame", "name": name, "parent": parent, "select": true, "x": 0, "y": 0, "width": geometry_w, "height": 120,
                "props": {"layout": "vertical", "gap": gap, "padding": pad, "sizingH": "fill", "sizingV": "fixed", "fill": fill, "stroke": edge, "locked": true,
                    "pluginData": {"oneos.id": oneos_id, "oneos.node": node.to_string(), "oneos.locked": "1"}}}),
        );
        let made = made.and_then(|r| {
            let id = r.get("id").and_then(Value::as_u64).ok_or_else(|| EngineError::Other("section.insert: the layer was not made".into()))?;
            self.execute(
                "node.create",
                &json!({"type": "text", "name": "locked label", "parent": id, "select": false, "text": format!("{shown} (edit in the panel)"),
                    "props": {"fontSize": size, "fill": ink, "sizingH": "fill", "textAutoResize": "height", "pluginData": {"oneos.decor": "1"}}}),
            )?;
            if let Some(index) = args.get("index").and_then(Value::as_u64) {
                self.execute("node.reorder", &json!({"id": id, "index": index}))?;
            }
            Ok(id)
        });
        self.host_trusted = was;
        let id = made.map_err(|e| e.to_string())?;
        Ok(json!({"id": id, "oneosId": oneos_id, "c": c}))
    }

    /// `bind.field {id, prop, field, scope?}`: record that a layer's property is fed by a OneOS field. The bridge turns it into the
    /// page's template (`{$record.x}`, `{$row.x}`) and the compose validator judges it, so a field compose cannot hold is refused.
    pub(crate) fn bind_field(&mut self, args: &Value) -> std::result::Result<Value, String> {
        let id = args.get("id").and_then(Value::as_u64).ok_or("bind.field: `id` is the layer's number")?;
        let prop = args.get("prop").and_then(Value::as_str).unwrap_or("");
        if !["text", "title", "subtitle", "label", "alt", "value"].contains(&prop) {
            return Err(format!("bind.field: `prop` is one of text, title, subtitle, label, alt, value (got '{prop}')"));
        }
        let field = args.get("field").and_then(Value::as_str).unwrap_or("");
        let segs: Vec<&str> = field.split('.').collect();
        if segs.len() > 6
            || segs.iter().any(|s| s.is_empty() || s.len() > 64 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
        {
            return Err(format!("bind.field: `field` is a dotted field path of letters, digits, _ and - (got '{field}')"));
        }
        let scope = args.get("scope").and_then(Value::as_str).unwrap_or("row");
        if !["row", "record", "me", "org", "space"].contains(&scope) {
            return Err(format!("bind.field: `scope` is row, record, me, org or space (got '{scope}')"));
        }
        let mut list: Vec<Value> = {
            let doc = self.doc().map_err(|e| e.to_string())?;
            if !doc.contains(NodeId(id)) {
                return Err(format!("no layer {id}"));
            }
            if !on_page(doc, NodeId(id)) {
                return Err(format!("bind.field: layer {id} is not on a compose page; only a page's layers carry fields"));
            }
            doc.node(NodeId(id))
                .and_then(|n| n.plugin_data.get(BIND_KEY))
                .and_then(|s| serde_json::from_str::<Vec<Value>>(s).ok())
                .unwrap_or_default()
        };
        list.retain(|b| b.get("prop").and_then(Value::as_str) != Some(prop));
        list.push(json!({"prop": prop, "field": field, "scope": scope}));
        let was = std::mem::replace(&mut self.host_trusted, true);
        let r = self.execute("node.set", &json!({"id": id, "props": {"pluginData": {BIND_KEY: Value::Array(list).to_string()}}}));
        self.host_trusted = was;
        r.map(|_| json!({"id": id, "prop": prop, "field": field, "scope": scope})).map_err(|e| e.to_string())
    }
}
