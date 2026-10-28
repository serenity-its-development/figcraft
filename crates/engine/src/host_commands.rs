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
    // Direct manipulation: the canvas gestures as numbers (a drag is `transform.move` / `transform.moveTo`, a resize handle is `transform.resize`,
    // the arrow keys are `transform.nudge`, a marquee is `select.area`, a click through the stack is `select.layersAt`, snapping to the pixel grid is
    // `arrange.roundToPixels`). Each acts at once on the selection or on `ids`, and a run is judged and taken back as the others are.
    "transform.nudgeSize",
    "transform.scale",
    "arrange.roundToPixels",
    "select.area",
    "select.layersAt",
    "select.inverse",
    "select.next",
    "select.previous",
    "select.matching",
    "select.sameFill",
    "select.sameStroke",
    "select.sameEffect",
    "select.sameText",
    "select.sameFont",
    "select.sameInstance",
    "select.sameProperties",
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
    // {node, parent, index?, name?}: a locked OneOS block (form, live list or table, metric ...) placed on the Expanded frame of a compose
    // page as a locked component instance; `node` is the compose node as JSON text and the compose validator judges it
    // (`design.section.insert`). Its properties are edited in the OneOS panel, never on the canvas.
    "section.insert",
];

/// Every command the app has and the assistant may NOT run, with the reason: together with `ASSISTANT_COMMANDS` and `ARGUMENT_COMMANDS` this is the whole command
/// catalog (a test holds the three to it, so a command can neither be forgotten nor listed twice).
pub const LEFT_OUT: &[(&str, &[&str])] = &[
    (
        "Files and addresses move only through the host: open, save, export, print, close, recover and the like are the host's, never a command id the assistant runs; so are the commands that read a file, a picture, a sound, a video, a font or a web address.",
        &[
            "image.place",
            "export.add",
            "export.update",
            "export.remove",
            "file.new",
            "file.newBoard",
            "file.open",
            "file.openRecent",
            "file.save",
            "file.saveAs",
            "file.export",
            "file.exportLayers",
            "file.exportSvg",
            "file.exportFramesPdf",
            "file.import",
            "file.placeImage",
            "file.close",
            "file.moveTab",
            "file.recover",
            "file.rename",
            "file.colorProfile",
            "file.duplicate",
            "file.revert",
            "file.saveVersion",
            "file.versionHistory",
            "libraries.open",
            "variable.export",
            "variable.import",
            "instance.place",
            "libraries.publish",
            "library.publish",
            "library.publishToServer",
            "library.place",
            "library.import",
            "codeConnect.export",
            "codeConnect.import",
            "plugin.list",
            "plugin.import",
            "plugin.remove",
            "plugin.run",
            "plugin.ui",
            "plugin.event",
            "plugin.close",
            "plugins.runLast",
            "plugins.manage",
            "plugin.directory.list",
            "plugin.directory.install",
            "plugin.directory.publish",
            "plugin.directory.unpublish",
            "widget.insert",
            "template.list",
            "file.newFromTemplate",
        ],
    ),
    (
        "Undo, redo and the history belong to the person: a run is one undo step, and undoing it is theirs.",
        &[
            "edit.undo",
            "edit.redo",
            "edit.repeat",
            "version.list",
            "version.restore",
            "version.rename",
            "version.delete",
            "version.preview",
            "version.autosave",
            "version.exitPreview",
            "version.duplicate",
        ],
    ),
    (
        "The system clipboard is the person's (copy, cut and paste read or write what they hold outside the app), and so are the format painters.",
        &[
            "edit.copy",
            "edit.cut",
            "edit.paste",
            "edit.pasteDesignLayers",
            "edit.pasteOver",
            "edit.pasteReplace",
            "edit.copyProperties",
            "edit.pasteProperties",
        ],
    ),
    (
        "The window, its appearance, the preferences, collaboration, plug-ins, comments on a shared file, developer mode, the prototype player and the help are the person's, not the document's.",
        &[
            "command.batch",
            "dev.resolveChange",
            "dev.compare",
            "dev.focus",
            "dev.unfocus",
            "doc.switch",
            "doc.nextTab",
            "doc.previousTab",
            "doc.list",
            "doc.home",
            "view.zoomIn",
            "view.zoomOut",
            "view.zoom100",
            "view.zoom50",
            "view.zoom200",
            "view.zoomFit",
            "view.zoomSelection",
            "view.set",
            "view.zoomAt",
            "view.get",
            "view.prevFrame",
            "view.nextFrame",
            "view.prevPage",
            "view.nextPage",
            "view.pixelGrid",
            "view.layoutGrids",
            "view.rulers",
            "view.slices",
            "view.comments",
            "view.multiplayerCursors",
            "view.pan",
            "view.outlines",
            "view.outlinesSelection",
            "view.outlinesHiddenLayers",
            "view.outlinesObjectBounds",
            "view.maskOutlines",
            "view.pixelPreview",
            "view.frameOutlines",
            "view.proofSrgb",
            "view.snapGeometry",
            "view.snapObjects",
            "view.snapPixel",
            "view.toggleUi",
            "view.fullscreen",
            "view.layers",
            "view.assets",
            "view.design",
            "view.prototype",
            "view.designPrototype",
            "view.devMode",
            "view.leftSidebar",
            "view.panelWidths",
            "window.new",
            "window.focus",
            "view.nextRegion",
            "view.previousRegion",
            "view.contextMenu",
            "view.themeLight",
            "view.themeDark",
            "view.themeSystem",
            "pref.keepTool",
            "pref.highlightHover",
            "pref.renameDuplicates",
            "pref.showDimensions",
            "pref.numberKeysOpacity",
            "pref.reduceMotion",
            "pref.uiScale",
            "pref.autoSave",
            "pref.startOnHome",
            "pref.nudge",
            "pref.userName",
            "pref.publishServer",
            "pref.language",
            "app.preferences",
            "app.actions",
            "help.shortcuts",
            "help.about",
            "help.page",
            "help.gettingStarted",
            "proto.present",
            "proto.preview",
            "proto.closePreview",
            "proto.presentOptions",
            "proto.linkNotice",
            "comment.add",
            "comment.reply",
            "comment.edit",
            "comment.react",
            "comment.resolve",
            "comment.move",
            "comment.delete",
            "comment.markRead",
            "comment.open",
            "comment.next",
            "comment.zoom",
            "comment.link",
            "comment.people",
            "comment.list",
            "inspect.code",
            "inspect.readyForDev",
            "inspect.assets",
            "inspect.properties",
            "inspect.componentCode",
            "annotation.add",
            "annotation.edit",
            "annotation.remove",
            "annotation.list",
            "annotation.categories",
            "annotation.addCategory",
            "annotation.editCategory",
            "annotation.removeCategory",
            "devResource.add",
            "devResource.edit",
            "devResource.remove",
            "devResource.list",
            "devMode.addMeasurement",
            "devMode.removeMeasurement",
            "devMode.measurements",
            "measure.distances",
            "measure.padding",
            "collab.diff",
            "branch.create",
            "branch.list",
            "branch.open",
            "branch.review",
            "branch.updateFromMain",
            "branch.merge",
            "branch.archive",
            "collab.host",
            "collab.share",
            "collab.stop",
            "collab.join",
            "collab.leave",
            "collab.publish",
            "collab.serverInfo",
            "collab.manageFile",
            "collab.report",
            "collab.peers",
            "collab.follow",
            "collab.removePerson",
            "collab.cursor",
            "collab.cursorChat",
            "collab.spotlight",
            "collab.files",
            "collab.openFile",
            "collab.forgetFile",
            "collab.recoverFile",
            "collab.registerLinks",
            "board.sticky",
            "board.nextSticky",
            "board.shape",
            "board.connector",
            "board.label",
            "board.stamp",
            "board.ink",
            "board.setColor",
            "board.setStyle",
        ],
    ),
    (
        "Interactive: it begins a pointer gesture or an editing session (editing a path's points, drawing, selecting text in place), which only a person's pointer or keyboard can finish.",
        &[
            "selection.selectColor",
            "object.setThumbnail",
            "tool.set",
            "tool.place",
            "tool.pencilBrush",
            "tool.scaleAnchor",
            "tool.move",
            "tool.scale",
            "tool.hand",
            "tool.frame",
            "tool.section",
            "tool.slice",
            "tool.rectangle",
            "tool.line",
            "tool.arrow",
            "tool.ellipse",
            "tool.polygon",
            "tool.star",
            "tool.pen",
            "tool.pencil",
            "tool.text",
            "tool.textOnPath",
            "tool.comment",
            "tool.shapeBuilder",
            "text.preview",
            "text.endPreview",
            "text.edit.begin",
            "text.edit.insert",
            "text.edit.key",
            "text.edit.caret",
            "text.edit.selectWord",
            "text.edit.selectLine",
            "text.edit.preedit",
            "text.edit.selection",
            "text.edit.end",
            "vector.edit.begin",
            "vector.edit.end",
            "vector.edit.state",
            "vector.edit.tool",
            "vector.edit.select",
            "vector.edit.selectAll",
            "vector.edit.moveVertices",
            "vector.edit.nudge",
            "vector.edit.position",
            "vector.edit.setHandle",
            "vector.edit.moveHandles",
            "vector.edit.mirroring",
            "vector.edit.smooth",
            "vector.edit.bend",
            "vector.edit.addVertex",
            "vector.edit.delete",
            "vector.edit.cornerRadius",
            "vector.edit.strokeCap",
            "vector.edit.strokeJoin",
            "vector.edit.paint",
            "vector.edit.cut",
            "vector.edit.erase",
            "vector.edit.connect",
            "vector.edit.lasso",
            "vector.edit.pen",
            "vector.edit.penEnd",
            "vector.edit.bounds",
            "vector.edit.resize",
            "vector.edit.rotate",
            "vector.edit.transform",
            "vector.edit.widthPoint",
            "shapeBuilder.regions",
            "shapeBuilder.merge",
            "shapeBuilder.remove",
            "shapeBuilder.mode",
            "tool.sticky",
            "tool.shape",
            "tool.connector",
            "tool.marker",
            "tool.highlighter",
            "tool.eraser",
            "tool.stamp",
        ],
    ),
    (
        "Takes arguments whose shape has not yet been checked against a run (a shape, a range, a name, a setting), so the assistant does not run it yet; every command that acts at once on the selection is in the list.",
        &[
            "vector.create",
            "frame.swapOrientation",
            "frame.presets",
            "edit.findReplace",
            "edit.setDefaultProperties",
            "object.batchRename",
            "object.hideOthers",
            "object.opacityKeys",
            "object.removeFill",
            "object.removeStroke",
            "object.swapFillStroke",
            "layers.collapseAll",
            "fill.pattern",
            "stroke.widthProfile",
            "stroke.brush",
            "grid.add",
            "grid.remove",
            "fill.move",
            "stroke.move",
            "effect.move",
            "grid.move",
            "selection.colors",
            "selection.setPaint",
            "selection.applyColorStyle",
            "selection.bindColorVariable",
            "selection.detachColor",
            "arrange.reorder",
            "arrange.packH",
            "arrange.packV",
            "arrange.front",
            "arrange.forward",
            "arrange.backward",
            "arrange.back",
            "object.flipH",
            "object.flipV",
            "object.rotate180",
            "object.rotateLeft",
            "object.rotateRight",
            "transform.rotate",
            "transform.rotationOrigin",
            "transform.setRotationOrigin",
            "object.convertToGroup",
            "object.convertToFrame",
            "object.convertToSection",
            "object.wrapInSection",
            "object.mask",
            "object.releaseMask",
            "bool.union",
            "bool.subtract",
            "bool.intersect",
            "bool.exclude",
            "bool.flatten",
            "vector.flatten",
            "vector.outlineStroke",
            "layout.suggest",
            "object.moveToPage",
            "object.rasterize",
            "vector.reverse",
            "vector.fillRule",
            "vector.reverseLoop",
            "vector.simplify",
            "vector.offset",
            "vector.join",
            "vector.smoothJoin",
            "section.toggleContents",
            "page.delete",
            "page.duplicate",
            "page.move",
            "page.setBackground",
            "page.list",
            "guide.add",
            "guide.move",
            "guide.remove",
            "guide.select",
            "guide.clear",
            "edit.pickColor",
            "text.onPath",
            "text.pathOptions",
            "text.removeFromPath",
            "text.bold",
            "text.italic",
            "text.underline",
            "text.strikethrough",
            "text.link",
            "text.bulletList",
            "text.numberList",
            "text.sizeUp",
            "text.sizeDown",
            "text.weightUp",
            "text.weightDown",
            "text.lineHeightUp",
            "text.lineHeightDown",
            "text.trackingUp",
            "text.trackingDown",
            "text.caseOriginal",
            "text.caseUpper",
            "text.caseLower",
            "text.caseTitle",
            "text.caseSmallCaps",
            "text.case",
            "text.decoration",
            "text.listStyle",
            "text.paragraphIndent",
            "text.paragraphSpacing",
            "text.maxLines",
            "text.setFeatures",
            "text.setVariations",
            "text.listIndent",
            "text.listOutdent",
            "text.fonts",
            "text.rescanFonts",
            "text.loadFont",
            "text.missingFonts",
            "text.refit",
            "paint.documentColors",
            "style.detach",
            "style.bind",
            "style.duplicate",
            "style.move",
            "style.renameFolder",
            "style.delete",
            "style.list",
            "variable.addMode",
            "variable.delete",
            "variable.setMode",
            "variable.list",
            "image.adjust",
            "image.savePreset",
            "image.deletePreset",
            "image.presets",
            "variable.update",
            "variable.duplicate",
            "variable.renameMode",
            "variable.removeMode",
            "variable.moveMode",
            "variable.renameCollection",
            "variable.deleteCollection",
            "variable.resetOverride",
            "component.createMultiple",
            "component.combineVariants",
            "component.addVariant",
            "component.exposeInstance",
            "instance.exposed",
            "component.makeSlot",
            "component.removeSlot",
            "instance.slotInsert",
            "instance.resetSlot",
            "component.hideWhenPublishing",
            "component.detach",
            "component.resetInstance",
            "component.resetOverride",
            "component.goToMain",
            "component.pushChanges",
            "component.restoreMain",
            "instance.swapAt",
            "instance.setProperties",
            "component.addProperty",
            "component.editProperty",
            "component.deleteProperty",
            "component.bindProperty",
            "component.properties",
            "component.list",
            "library.list",
            "library.add",
            "library.usage",
            "library.swap",
            "library.remove",
            "library.updates",
            "library.acceptUpdates",
            "library.swapAt",
            "library.applyStyle",
            "library.importVariable",
            "library.bindVariable",
            "library.aliasVariable",
            "library.setVariableMode",
            "library.extendCollection",
            "proto.interaction.add",
            "proto.interaction.clear",
            "proto.interaction.remove",
            "proto.interaction.set",
            "proto.flow.add",
            "proto.flow.rename",
            "proto.flow.remove",
            "proto.settings",
            "proto.action.add",
            "proto.action.set",
            "proto.action.remove",
            "proto.action.move",
            "proto.conditional.addBlock",
            "proto.conditional.setCondition",
            "proto.conditional.removeBlock",
            "proto.evaluate",
            "vector.deleteHeal",
            "component.playground",
            "codeConnect.set",
            "codeConnect.remove",
            "widget.click",
            "widget.list",
            "data.fill",
            "text.fit",
        ],
    ),
];

/// Every command id a run may name, in the order the host's registry declares them.
pub fn allowed() -> Vec<&'static str> {
    ASSISTANT_COMMANDS.iter().chain(ARGUMENT_COMMANDS).copied().collect()
}

const MAX_STEPS: usize = 50;
/// The host drawing a whole page (a trusted run) may state more: a page of 200 layers is some 400 steps.
const MAX_TRUSTED_STEPS: usize = 4000;
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
        let max = if params.get("trusted").and_then(Value::as_bool) == Some(true) { MAX_TRUSTED_STEPS } else { MAX_STEPS };
        if steps.len() > max {
            return Err(format!("run-too-long: a run is at most {max} steps"));
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
            "section.insert" => {
                let args = crate::resolve_step_refs(args, &earlier, step, 0).map_err(|e| e.to_string())?;
                self.section_insert(&args)
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

    #[test]
    fn the_allow_list_and_the_commands_left_out_are_the_whole_catalog_with_nothing_twice() {
        let all: Vec<String> = crate::cmd::all_commands().iter().map(|c| c.id.to_string()).collect();
        let mut seen = std::collections::HashSet::new();
        for id in ASSISTANT_COMMANDS {
            assert!(all.iter().any(|c| c == id), "{id} is not a command of the app");
            assert!(seen.insert(id.to_string()), "{id} is listed twice");
        }
        // The window's own commands stand beside the registry's: they are not in it.
        for id in ARGUMENT_COMMANDS {
            assert!(seen.insert(id.to_string()), "{id} is listed twice");
        }
        for (reason, ids) in LEFT_OUT {
            assert!(reason.len() > 20, "a reason: {reason}");
            for id in *ids {
                assert!(all.iter().any(|c| c == id), "{id} is left out but is not a command of the app");
                assert!(seen.insert(id.to_string()), "{id} is both allowed and left out, or left out twice");
            }
        }
        let missing: Vec<&String> = all.iter().filter(|c| !seen.contains(c.as_str())).collect();
        assert!(missing.is_empty(), "commands neither allowed nor left out (with a reason): {missing:?}");
    }

    /// A layer's geometry by name, read back with `inspect`.
    fn geometry(s: &mut Session, name: &str) -> (f64, f64, f64, f64) {
        fn find<'a>(n: &'a Value, name: &str) -> Option<&'a Value> {
            if n.get("name").and_then(Value::as_str) == Some(name) {
                return Some(n);
            }
            n.get("children").and_then(Value::as_array).and_then(|c| c.iter().find_map(|k| find(k, name)))
        }
        let r = s.run_command_batch(&json!({"steps": [{"name": "inspect", "args": {"depth": 6}}]})).expect("read back");
        let tree = &r["steps"][0]["result"]["tree"];
        let n = find(tree, name).unwrap_or_else(|| panic!("no layer {name} in {tree}"));
        let f = |k: &str| n.get(k).and_then(Value::as_f64).unwrap_or_else(|| panic!("{k} in {n}"));
        (f("x"), f("y"), f("width"), f("height"))
    }

    #[test]
    fn the_canvas_gestures_run_as_numbers_in_one_undo_step() {
        let mut s = session();
        s.run_command_batch(&json!({"label": "Two boxes", "steps": [
            {"name": "frame.create", "args": {"x": 0, "y": 0, "width": 400, "height": 300, "name": "Page"}},
            {"name": "shape.rectangle", "args": {"x": 10.4, "y": 10.6, "width": 50, "height": 40, "name": "A"}},
            {"name": "shape.rectangle", "args": {"x": 120, "y": 50, "width": 60, "height": 20, "name": "B"}},
        ]}))
        .expect("two boxes");
        let before = s.undo_depth().0;
        // A drag, a resize from the bottom-right handle, a nudge by the arrow key, a marquee over both and an alignment, in one run.
        let r = s
            .run_command_batch(&json!({"label": "Arrange", "steps": [
                {"name": "select.nodes", "args": {"names": ["A"]}},
                {"name": "transform.move", "args": {"dx": 30, "dy": 5}},
                {"name": "transform.resize", "args": {"width": 80, "height": 60, "anchor": "tl"}},
                {"name": "transform.nudge", "args": {"dx": 1, "dy": 1}},
                {"name": "arrange.roundToPixels"},
                {"name": "select.area", "args": {"x": 0, "y": 0, "width": 399, "height": 299, "deep": true}},
                {"name": "align.top"},
            ]}))
            .expect("arrange");
        assert_eq!(r["undoStep"], json!(true));
        assert_eq!(s.undo_depth().0, before + 1, "seven steps are one undo step");
        let (ax, ay, aw, ah) = geometry(&mut s, "A");
        assert_eq!((aw, ah), (80.0, 60.0), "resized");
        assert_eq!(ax.fract(), 0.0, "snapped to whole pixels");
        assert_eq!(ay.fract(), 0.0, "snapped to whole pixels");
        assert!(ax > 40.0, "dragged right of where it was: {ax}");
        let (_, by, ..) = geometry(&mut s, "B");
        assert_eq!(by, ay, "the marquee selected both and align.top lined them up");
        // One undo puts both back where they started.
        s.execute("edit.undo", &json!({})).unwrap();
        let (ax, ay, aw, ah) = geometry(&mut s, "A");
        assert_eq!((ax, ay, aw, ah), (10.4, 10.6, 50.0, 40.0));
    }

    #[test]
    fn a_move_of_a_layer_that_is_not_there_takes_the_whole_run_back() {
        let mut s = session();
        s.run_command_batch(&json!({"steps": [{"name": "shape.rectangle", "args": {"x": 0, "y": 0, "width": 10, "height": 10, "name": "A"}}]}))
            .unwrap();
        let e = s
            .run_command_batch(&json!({"steps": [
                {"name": "select.nodes", "args": {"names": ["A"]}},
                {"name": "transform.move", "args": {"dx": 100, "dy": 0}},
                {"name": "transform.move", "args": {"ids": [999_999_999], "dx": 1, "dy": 1}},
            ]}))
            .expect_err("fails");
        assert!(e.starts_with("step-failed: step 3"), "{e}");
        assert_eq!(geometry(&mut s, "A").0, 0.0);
    }
}
