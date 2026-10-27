//! The assistant's command surface for the OneOS host bridge (`craft:command {method: "command.batch"}`).
//!
//! The host relays an allow-listed run: `{label, steps: [{name, args?}]}`. The whole run is judged BEFORE anything runs (an id
//! outside the list, or an argument that names a file, refuses the run), every step runs through the same registry the menus,
//! the control channel and the MCP server use (so a step the document's state forbids is refused exactly as it would be for the
//! person), and the run is ONE undo step named by `label`: a step that fails takes back every step before it, so a run is all
//! or nothing. There is no file-path or dialog command in the list: files move only through the host.
//!
//! This list is the one declared in the host's `platform-craft` registry as `design` commands (pinned equal there).

use base64::Engine as _;
use serde_json::{Value, json};

use crate::Session;

/// Registry commands the assistant may run. Each acts at once on the document (no dialog, no file picker, no pointer gesture),
/// and none is Undo or Redo: undoing a run is the person's.
pub const ASSISTANT_COMMANDS: &[&str] = &[
    // Make things.
    "frame.create",
    "node.create",
    "text.create",
    "shape.rectangle",
    "shape.ellipse",
    "shape.polygon",
    "shape.star",
    "shape.line",
    "shape.arrow",
    "page.add",
    "page.rename",
    "page.select",
    // Structure: containers, order and auto layout.
    "layout.add",
    "layout.remove",
    "layout.setTrack",
    "object.group",
    "object.ungroup",
    "object.frameSelection",
    "object.removeFrame",
    "object.resizeToFit",
    "node.reparent",
    "node.reorder",
    "edit.duplicate",
    "edit.delete",
    // Properties: any layer property by name, then the paint and effect lists.
    "node.set",
    "object.rename",
    "object.toggleVisible",
    "object.toggleLock",
    "fill.add",
    "fill.set",
    "fill.remove",
    "stroke.add",
    "stroke.set",
    "stroke.remove",
    "effect.add",
    "effect.set",
    "effect.remove",
    // Place and size.
    "transform.moveTo",
    "transform.move",
    "transform.nudge",
    "transform.resize",
    "align.left",
    "align.hcenter",
    "align.right",
    "align.top",
    "align.vcenter",
    "align.bottom",
    "distribute.horizontal",
    "distribute.vertical",
    "arrange.spacing",
    "arrange.tidy",
    // Text.
    "text.set",
    "text.style",
    "text.alignLeft",
    "text.alignCenter",
    "text.alignRight",
    "text.alignJustify",
    // Styles, variables, components.
    "style.create",
    "style.update",
    "style.apply",
    "variable.createCollection",
    "variable.create",
    "variable.set",
    "variable.bind",
    "component.create",
    "instance.create",
    "instance.swap",
    // Choosing what the next step acts on.
    "select.set",
    "select.add",
    "select.remove",
    "select.all",
    "select.none",
    "select.children",
    "select.parent",
    "select.byName",
    // Read-only.
    "node.get",
];

/// Commands the window itself provides (they take arguments, which the registry's commands do not).
pub const ARGUMENT_COMMANDS: &[&str] = &[
    // {ids?: [layer ids], names?: [layer names on the current page], all?: bool}: select layers, so the commands that act on the
    // selection have something to act on.
    "select.nodes",
    // Read-only: the pages, the current page's layers as a tree (id, name, type, geometry, fills, text, layout) and the selection.
    "inspect",
    // Read-only: the current page (or `id`, or with {selection: true} what is selected) as a PNG, base64, at most 1600 pixels wide.
    "render",
    // {id, prop, field, scope?}: a layer's property is fed by a OneOS field (`design.bind.field`). Typed, so the oneos.* sidecar is
    // never written by a free `node.set` on a compose page.
    "bind.field",
];

/// Every command id a run may name, in the order the host's registry declares them.
pub fn allowed() -> Vec<&'static str> {
    ASSISTANT_COMMANDS.iter().chain(ARGUMENT_COMMANDS).copied().collect()
}

const MAX_STEPS: usize = 50;
const MAX_LABEL: usize = 120;
const MAX_INSPECT_DEPTH: usize = 8;
const MAX_RENDER_SIDE: f64 = 1600.0;
/// Argument names that would point a command at a file or a network address: refused anywhere in a step's arguments.
const FORBIDDEN_ARGS: &[&str] = &["path", "dir", "folder", "file", "url", "uri", "href", "server", "data"];

/// The first forbidden argument name anywhere in `v`.
fn forbidden_arg(v: &Value) -> Option<String> {
    match v {
        Value::Object(o) => o.iter().find_map(|(k, x)| if FORBIDDEN_ARGS.contains(&k.as_str()) { Some(k.clone()) } else { forbidden_arg(x) }),
        Value::Array(a) => a.iter().find_map(forbidden_arg),
        _ => None,
    }
}

impl Session {
    /// One `command.batch`: all of it, as ONE undo step, or none of it.
    pub fn run_command_batch(&mut self, params: &Value) -> Result<Value, String> {
        let label = params.get("label").and_then(Value::as_str).map(str::trim).unwrap_or("");
        if label.chars().count() > MAX_LABEL {
            return Err(format!("label-too-long: a run's label is at most {MAX_LABEL} characters"));
        }
        let steps = params.get("steps").and_then(Value::as_array).ok_or("run-empty: a run states its steps")?;
        if steps.is_empty() {
            return Err("run-empty: a run states at least one command".into());
        }
        if steps.len() > MAX_STEPS {
            return Err(format!("run-too-long: a run is at most {MAX_STEPS} steps"));
        }
        // Judge the whole run first; nothing has run when one id is outside the list.
        let mut named = Vec::with_capacity(steps.len());
        for step in steps {
            let name = step.get("name").and_then(Value::as_str).unwrap_or("");
            if !ASSISTANT_COMMANDS.contains(&name) && !ARGUMENT_COMMANDS.contains(&name) {
                return Err(format!("command-not-allowed: Design does not allow the command '{name}' -- nothing was run"));
            }
            let args = step.get("args").cloned().unwrap_or_else(|| json!({}));
            if let Some(k) = forbidden_arg(&args) {
                return Err(format!(
                    "argument-not-allowed: step '{name}' names '{k}', and a run does not reach files or addresses -- nothing was run"
                ));
            }
            named.push((name, args));
        }
        self.ds().map_err(|_| "no-document: no design file is open in this window".to_string())?;
        let depth = self.undo_depth().0;
        let label = if label.is_empty() { "Assistant" } else { label };
        // `trusted`: the host's own bridge (it writes the oneos.* sidecar); `dryRun`: run, read the compose pages back, and take it
        // ALL back (nothing lands, nothing reaches the room) so the host can judge the result with the compose validator first.
        let trusted = params.get("trusted").and_then(Value::as_bool) == Some(true);
        let dry = params.get("dryRun").and_then(Value::as_bool) == Some(true);
        let snapshot = self.ds().ok().map(|d| (d.doc.clone(), d.selection.clone(), d.page, d.uid));
        let was_trusted = std::mem::replace(&mut self.host_trusted, trusted);
        let mut tree: Option<Value> = None;
        // The engine's own grouping: the steps push no undo steps of their own, the run is ONE step named `label`, and a failing
        // step leaves the document, selection, page and redo steps as they were.
        let outcome = self.grouped(label, |s| {
            let mut results: Vec<Value> = Vec::with_capacity(named.len());
            s.defer_token_check = true;
            for (i, (name, args)) in named.iter().enumerate() {
                let result = s.run_step(&results, i, name, args).map_err(|e| {
                    s.defer_token_check = false;
                    crate::EngineError::Other(format!("step-failed: step {} ({name}): {e} -- nothing was changed", i + 1))
                })?;
                results.push(json!({ "name": name, "result": result }));
            }
            s.defer_token_check = false;
            // Tokens are judged on the run's END state: a frame is born with default values the run's later steps set right.
            s.check_compose_tokens("command.batch", &snapshot)?;
            if dry {
                tree = Some(s.compose_pages_json());
                return Err(crate::EngineError::Other("dry-run".into()));
            }
            Ok(results)
        });
        self.host_trusted = was_trusted;
        self.defer_token_check = false;
        if dry {
            return match (outcome, tree) {
                (Err(crate::EngineError::Other(m)), Some(t)) if m == "dry-run" => Ok(json!({ "dryRun": true, "pages": t })),
                (Err(e), _) => Err(e.to_string()),
                (Ok(_), _) => Err("dry-run: the run was not taken back".into()),
            };
        }
        let results = outcome.map_err(|e| e.to_string())?;
        Ok(json!({ "steps": results, "undoStep": self.undo_depth().0 > depth }))
    }

    /// Every compose page frame on the document's pages as the `inspect` tree (so the host can read the pages back).
    fn compose_pages_json(&self) -> Value {
        let Ok(doc) = self.doc() else { return Value::Array(vec![]) };
        let mut out = Vec::new();
        for page in &doc.pages {
            for c in doc.node(*page).map(|p| p.children.clone()).unwrap_or_default() {
                if doc.node(c).is_some_and(|n| n.plugin_data.contains_key(crate::compose_tokens::PAGE_KEY)) {
                    out.push(crate::inspect::node_json(doc, c, MAX_INSPECT_DEPTH));
                }
            }
        }
        Value::Array(out)
    }

    /// One step: `{"$step": n}` in its arguments names the result of the earlier step n (the layer it made).
    fn run_step(&mut self, results: &[Value], step: usize, name: &str, args: &Value) -> Result<Value, String> {
        let earlier: Vec<Value> = results.iter().map(|r| r["result"].clone()).collect();
        match name {
            "bind.field" => {
                let args = crate::resolve_step_refs(args, &earlier, step, 0).map_err(|e| e.to_string())?;
                self.bind_field(&args)
            }
            "select.nodes" | "inspect" | "render" => {
                let args = crate::resolve_step_refs(args, &earlier, step, 0).map_err(|e| e.to_string())?;
                match name {
                    "select.nodes" => self.select_nodes(&args),
                    "inspect" => self.inspect_design(&args),
                    _ => self.render_design(&args),
                }
            }
            other => self.batch_step(&earlier, step, other, args).map_err(|e| e.to_string()),
        }
    }

    fn select_nodes(&mut self, args: &Value) -> Result<Value, String> {
        let doc = self.doc().map_err(|e| e.to_string())?;
        let page = self.page().map_err(|e| e.to_string())?;
        let mut ids: Vec<u64> = Vec::new();
        if args.get("all").and_then(Value::as_bool) == Some(true) {
            ids.extend(doc.subtree(page).into_iter().filter(|i| *i != page).map(|i| i.0));
        }
        for id in args.get("ids").and_then(Value::as_array).into_iter().flatten() {
            let n = id
                .as_u64()
                .or_else(|| id.as_str().and_then(|s| s.parse::<figcraft_doc::NodeId>().ok().map(|n| n.0)))
                .ok_or("ids: layer ids are numbers")?;
            if !doc.contains(figcraft_doc::NodeId(n)) {
                return Err(format!("no layer {n}"));
            }
            ids.push(n);
        }
        for name in args.get("names").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            let found = doc.find_by_name(name, Some(page));
            if found.is_empty() {
                return Err(format!("no layer named '{name}' on this page"));
            }
            ids.extend(found.into_iter().map(|i| i.0));
        }
        self.execute("select.set", &json!({ "ids": ids })).map_err(|e| e.to_string())
    }

    fn inspect_design(&mut self, args: &Value) -> Result<Value, String> {
        let doc = self.doc().map_err(|e| e.to_string())?;
        let page = self.page().map_err(|e| e.to_string())?;
        let depth = args.get("depth").and_then(Value::as_u64).map_or(4, |d| (d as usize).min(MAX_INSPECT_DEPTH));
        let root = match args.get("id").and_then(Value::as_u64) {
            Some(n) if doc.contains(figcraft_doc::NodeId(n)) => figcraft_doc::NodeId(n),
            Some(n) => return Err(format!("no layer {n}")),
            None => page,
        };
        let pages: Vec<Value> = doc.pages.iter().filter_map(|p| doc.node(*p).map(|n| json!({"id": n.id, "name": n.name}))).collect();
        Ok(json!({
            "name": doc.name,
            "pages": pages,
            "page": page,
            "tree": crate::inspect::node_json(doc, root, depth),
            "selected": self.selection(),
            "undo": self.undo_labels(1).first().cloned(),
        }))
    }

    fn render_design(&mut self, args: &Value) -> Result<Value, String> {
        let doc = self.doc().map_err(|e| e.to_string())?;
        let page = self.page().map_err(|e| e.to_string())?;
        let ids: Vec<figcraft_doc::NodeId> = match args.get("id").and_then(Value::as_u64) {
            Some(n) if doc.contains(figcraft_doc::NodeId(n)) => vec![figcraft_doc::NodeId(n)],
            Some(n) => return Err(format!("no layer {n}")),
            None => {
                // The page, unless the run asks for what is selected.
                let sel = self.selection();
                if args.get("selection").and_then(Value::as_bool) == Some(true) && !sel.is_empty() {
                    sel
                } else {
                    doc.node(page).map(|n| n.children.clone()).unwrap_or_default()
                }
            }
        };
        if ids.is_empty() {
            return Err("nothing to render: the page is empty".into());
        }
        let setting = figcraft_doc::ExportSetting { format: figcraft_doc::ExportFormat::Png, ..Default::default() };
        let area = crate::cmd::file::export_area(doc, &ids, &setting).ok_or("nothing to render: the layers paint nothing")?;
        let scale = (MAX_RENDER_SIDE / area.width().max(area.height()).max(1.0)).min(1.0);
        let bytes = crate::cmd::file::export_bytes_with(doc, &ids, &setting, scale).map_err(|e| e.to_string())?;
        Ok(json!({ "mime": "image/png", "bytes": bytes.len(), "png": base64::engine::general_purpose::STANDARD.encode(&bytes) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::new()
    }

    #[test]
    fn every_listed_command_exists_in_the_registry() {
        let known: Vec<String> = crate::cmd::all_commands().iter().map(|c| c.id.to_string()).collect();
        for id in ASSISTANT_COMMANDS {
            assert!(known.iter().any(|k| k == id), "{id} is not a registry command");
        }
        for bad in [
            "file.save",
            "file.saveAs",
            "file.open",
            "file.export",
            "file.import",
            "file.placeImage",
            "edit.undo",
            "edit.redo",
            "proto.present",
            "library.publishToServer",
        ] {
            assert!(!allowed().contains(&bad), "{bad} must not be allowed");
        }
        for id in allowed() {
            assert!(
                !id.contains("file")
                    && !id.contains("path")
                    && !id.contains("open")
                    && !id.contains("save")
                    && !id.contains("export")
                    && !id.contains("import"),
                "{id}"
            );
        }
    }

    #[test]
    fn a_run_is_judged_whole_first_and_arguments_never_reach_files() {
        let mut s = session();
        let before = s.undo_depth();
        let refused = s.run_command_batch(&json!({"steps": [{"name": "frame.create"}, {"name": "file.save"}]})).expect_err("outside the list");
        assert!(refused.starts_with("command-not-allowed"), "{refused}");
        let path = s
            .run_command_batch(&json!({"steps": [{"name": "frame.create", "args": {"props": {"imagePath": 1, "x": {"path": "/etc/passwd"}}}}]}))
            .expect_err("path");
        assert!(path.starts_with("argument-not-allowed"), "{path}");
        assert_eq!(s.undo_depth(), before, "nothing ran");
    }

    #[test]
    fn a_run_is_one_undo_step_and_one_undo_takes_it_all_back() {
        let mut s = session();
        let before = s.undo_depth().0;
        let count = s.doc().unwrap().node_count();
        let ok = s
            .run_command_batch(&json!({"label": "Landing", "steps": [
                {"name": "frame.create", "args": {"x": 0, "y": 0, "width": 400, "height": 300, "name": "Page"}},
                {"name": "layout.add", "args": {"direction": "vertical"}},
                {"name": "text.create", "args": {"text": "Hello", "x": 10, "y": 10}},
                {"name": "inspect"}
            ]}))
            .expect("run");
        assert_eq!(ok["steps"].as_array().unwrap().len(), 4);
        assert_eq!(s.undo_depth().0, before + 1, "four steps are one undo step");
        assert_eq!(s.undo_labels(1), vec!["Landing".to_string()]);
        assert!(s.doc().unwrap().node_count() > count);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.doc().unwrap().node_count(), count);
    }

    #[test]
    fn a_failing_step_takes_back_the_steps_before_it() {
        let mut s = session();
        let count = s.doc().unwrap().node_count();
        let e = s
            .run_command_batch(&json!({"steps": [{"name": "frame.create"}, {"name": "node.set", "args": {"id": 999_999_999, "props": {"x": 1}}}]}))
            .expect_err("fails");
        assert!(e.starts_with("step-failed"), "{e}");
        assert_eq!(s.doc().unwrap().node_count(), count);
    }

    #[test]
    fn layers_are_selected_by_name_and_rendered() {
        let mut s = session();
        s.run_command_batch(&json!({"steps": [{"name": "frame.create", "args": {"x": 0, "y": 0, "width": 200, "height": 100, "name": "Card"}}]}))
            .unwrap();
        s.run_command_batch(
            &json!({"steps": [{"name": "select.nodes", "args": {"names": ["Card"]}}, {"name": "fill.set", "args": {"color": "#FF0000"}}]}),
        )
        .unwrap();
        let e = s.run_command_batch(&json!({"steps": [{"name": "select.nodes", "args": {"names": ["No such layer"]}}]})).expect_err("unknown name");
        assert!(e.contains("no layer named"), "{e}");
        let r = s.run_command_batch(&json!({"steps": [{"name": "render"}]})).unwrap();
        assert_eq!(r["steps"][0]["result"]["mime"], "image/png");
    }
}
