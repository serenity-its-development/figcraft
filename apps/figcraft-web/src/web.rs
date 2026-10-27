//! The browser shell: web `Services`, the eframe web runner, and (framed by OneOS) the host bridge and live editing.

use figcraft_engine::Session;
use figcraft_ui_egui::{ContentsFuture, FigApp, Services};
use wasm_bindgen::JsCast as _;

const CANVAS_ID: &str = "figcraft_canvas";
const LOADING_ID: &str = "figcraft_loading";

pub fn start() {
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    wasm_bindgen_futures::spawn_local(async {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            log::error!("no document");
            return;
        };
        let Some(canvas) = document.get_element_by_id(CANVAS_ID).and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok()) else {
            log::error!("missing <canvas id=\"{CANVAS_ID}\">");
            return;
        };
        // OneOS host bridge: the frame page's loader declares the ONE parent origin this editor answers to
        // (`globalThis.__craftHost`); with none declared the editor runs unframed and saves download.
        let host = crate::host::Bridge::new();
        let host_for_app = host.clone();
        let mut options = eframe::WebOptions::default();
        // `?webgl`: force the WebGL2 backend (troubleshooting, and headless browsers whose WebGPU
        // adapter request returns nothing).
        if query().contains("webgl")
            && let eframe::egui_wgpu::WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup
        {
            create.instance_descriptor.backends = eframe::wgpu::Backends::GL;
        }
        let result = eframe::WebRunner::new()
            .start(
                canvas,
                options,
                Box::new(move |cc| {
                    if let Some(rs) = &cc.wgpu_render_state {
                        log::info!("figcraft-web: wgpu backend {:?}", rs.adapter.get_info().backend);
                    }
                    let reduce_motion = web_sys::window().and_then(|w| w.match_media("(prefers-reduced-motion: reduce)").ok().flatten());
                    let bridge = host_for_app.clone();
                    // Framed by OneOS the host sends the file: until it does, the home screen.
                    let mut session = if bridge.is_some() { Session::empty() } else { Session::new() };
                    if bridge.is_none() {
                        // Live sessions: the browser joins sessions a desktop app hosts; whatever arrives
                        // on the socket wakes the page for a frame. (Framed by OneOS, live editing is the host's room, never
                        // FigCraft's own server.)
                        let ctx = cc.egui_ctx.clone();
                        session.live.install(Box::new(figcraft_collab_net::NetLink::new(std::sync::Arc::new(move || ctx.request_repaint()))));
                    }
                    if let Some(b) = &bridge {
                        b.connect(cc.egui_ctx.clone());
                    }
                    let mut fig = FigApp::new(session, Box::new(WebServices { reduce_motion, ctx: cc.egui_ctx.clone(), hosted: bridge.is_some() }));
                    // `#join=<link>`: join that session, as opening an invite link does on the desktop.
                    let hash = web_sys::window().and_then(|w| w.location().hash().ok()).unwrap_or_default();
                    if bridge.is_none()
                        && let Some(link) = crate::query::join_link(&hash)
                    {
                        let link = js_sys::decode_uri_component(link).ok().and_then(|s| s.as_string()).unwrap_or_else(|| link.to_string());
                        figcraft_ui_egui::live::open_invite(&mut fig, &link);
                    }
                    let collab = bridge.clone().map(crate::collab::Collab::new);
                    if let Some(b) = &bridge {
                        b.post_ready();
                    }
                    let handoff = bridge.clone().map(|bridge| (bridge, crate::handoff::Handoff::new()));
                    Ok(Box::new(WebShell { fig, bridge, collab, reported: (String::new(), None), reported_select: None, handoff }))
                }),
            )
            .await;
        if let Some(el) = document.get_element_by_id(LOADING_ID) {
            match result {
                Ok(()) => el.remove(),
                Err(e) => {
                    if let Some(b) = &host {
                        b.post_failed(&format!("{e:?}"));
                    }
                    el.set_inner_html(&format!("<p>Design failed to start: {e:?}</p><p>A browser with WebGPU or WebGL2 is required.</p>"))
                }
            }
        }
    });
}

fn query() -> String {
    web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default()
}

struct WebShell {
    fig: FigApp,
    /// OneOS host bridge, when framed by it.
    bridge: Option<crate::host::Bridge>,
    /// Live editing through the host (`collab`), when framed by it.
    collab: Option<crate::collab::Collab>,
    /// The last (session, dirty) the host was told: a new open starts again.
    reported: (String, Option<bool>),
    /// The `oneos.id` of the single selected layer the host was last told of (`craft:select`), "" for none.
    reported_select: Option<String>,
    /// The progressive open's hand-over to the host (`craft:opened`, the first view's place, `craft:viewed`), when framed by it.
    handoff: Option<(crate::host::Bridge, crate::handoff::Handoff<Option<u64>>)>,
}

impl eframe::App for WebShell {
    // `FigApp::ui` runs its own per-frame logic first (fonts, theme), like the desktop app.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some((bridge, handoff)) = self.handoff.as_mut() {
            handoff.before(bridge, self.fig.session.ds().ok().map(|d| d.uid));
        }
        self.host_events(ui.ctx());
        self.fig.ui(ui);
        self.after_frame();
        if let Some((bridge, handoff)) = self.handoff.as_mut() {
            let session = &mut self.fig.session;
            let shown = session.ds().ok().map(|d| d.uid);
            if handoff.after(bridge, shown, |view| {
                if let Err(e) = session.open_at_first_view(&view.selection, view.x_fraction(), view.y_fraction()) {
                    log::warn!("the first view's place was not applied: {e}");
                }
            }) {
                ui.ctx().request_repaint();
            }
        }
    }
}

impl WebShell {
    /// What the host said since the last frame: files to open, the live channel, the assistant's runs.
    fn host_events(&mut self, ctx: &egui::Context) {
        let Some(bridge) = self.bridge.clone() else { return };
        for event in bridge.take_events() {
            match event {
                crate::host::Event::Opened(opened, bytes) => self.open_host_file(&opened, &bytes),
                crate::host::Event::Command { id, method, params } => {
                    let outcome = if method == "command.batch" {
                        // A view-only window still DRAWS the page the host states (a trusted run: the host's own bridge, nothing is saved or
                        // sent to the room from a read-only window); the assistant's runs are refused.
                        if bridge.opened().read_only && params.get("trusted").and_then(serde_json::Value::as_bool) != Some(true) {
                            Err("read-only: this design is open to view, so a run cannot change it".to_string())
                        } else {
                            self.fig.session.run_command_batch(&params)
                        }
                    } else if method == "constrain.set" {
                        // The host states the theme's tokens for a compose-bound document (null lifts the constraint).
                        self.fig.session.set_compose_tokens(params.get("tokens"))
                    } else {
                        Err(format!("unknown-method: this editor does not take `{method}`"))
                    };
                    bridge.answer(&id, outcome);
                }
                other => {
                    if let Some(collab) = self.collab.as_mut() {
                        collab.event(other, &mut self.fig);
                    }
                }
            }
            ctx.request_repaint();
        }
    }

    /// The host's file becomes the document in front, saved under its own name (a Save is a new version of it).
    fn open_host_file(&mut self, opened: &crate::host::Opened, bytes: &[u8]) {
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        let session = &mut self.fig.session;
        // The file that was open before is the host's no longer.
        session.collab_undo = false;
        session.collab_undo_requests.clear();
        let params = serde_json::json!({"data": data, "name": opened.name});
        // No bytes: a Pages & apps page, which the host draws from its compose draft by commands (it saves no .figcraft).
        let opening = if bytes.is_empty() { session.execute("file.new", &serde_json::json!({"name": opened.name})) } else { session.execute("file.open", &params) };
        match opening {
            Ok(_) => {
                if let Err(e) = session.bind_host_name(None, &opened.name) {
                    log::error!("{e}");
                }
                self.fig.ui.home = false;
                if let Some(collab) = self.collab.as_mut() {
                    collab.opened(opened, &mut self.fig.session);
                }
            }
            Err(e) => {
                log::error!("the host's file did not open: {e}");
                if let Some(b) = &self.bridge {
                    b.post_failed(&format!("{} did not open: {e}", opened.name));
                }
                self.fig.toast(e.to_string());
            }
        }
    }

    /// After the app's own pass: this frame's edits go to the room, and unsaved changes (or none) to the host.
    fn after_frame(&mut self) {
        let Some(bridge) = self.bridge.clone() else { return };
        if let Some(collab) = self.collab.as_mut() {
            collab.post(&mut self.fig);
        }
        let session = bridge.opened().session;
        if session.is_empty() {
            return;
        }
        if self.reported.0 != session {
            self.reported = (session.clone(), None);
            self.reported_select = None;
        }
        // A compose-bound layer selected (or a double-click's selection) opens its OneOS properties panel in the host.
        let selected = self.fig.session.selected_oneos_id();
        if self.reported_select.as_deref() != Some(selected.as_str()) && bridge.post_select(&selected) {
            self.reported_select = Some(selected);
        }
        let dirty = self.fig.session.host_dirty();
        if self.reported.1 != Some(dirty) && bridge.post_dirty(dirty) {
            self.reported.1 = Some(dirty);
        }
    }
}

/// Browser services. Files are opened and imported through the browser's file picker, which hands
/// over their contents (pickers that need a path return nothing, the trait's defaults); files are
/// saved as downloads. Copies go to the system clipboard through egui's output; pastes arrive as
/// paste events with their text (a page can't read the clipboard when it likes).
struct WebServices {
    /// `prefers-reduced-motion: reduce` (it follows the setting while the page is open).
    reduce_motion: Option<web_sys::MediaQueryList>,
    ctx: egui::Context,
    /// Framed by OneOS: a Save as and an Export go to the host (a copy), not to a download.
    hosted: bool,
}

impl Services for WebServices {
    /// An `<input type="file">` taking `exts`, clicked; its file is read once picked.
    fn pick_open_contents(&mut self, _title: &str, exts: &[&str]) -> Option<ContentsFuture> {
        let accept = exts.iter().map(|e| format!(".{e}")).collect::<Vec<_>>().join(",");
        Some(Box::pin(async move {
            pick_file(&accept).await.unwrap_or_else(|e| {
                log::error!("couldn't open a file: {e:?}");
                None
            })
        }))
    }
    /// Framed by OneOS the host takes the file: a copy under the name the person gave (never a new version of the opened file).
    fn pick_save(&mut self, _title: &str, name: &str, _exts: &[&str]) -> figcraft_ui_egui::DialogFuture {
        let chosen = self.hosted.then(|| std::path::PathBuf::from(name));
        Box::pin(std::future::ready(chosen))
    }
    fn downloads(&self) -> bool {
        true
    }
    /// A Blob behind a temporary `<a download>` link, clicked and removed.
    fn download(&mut self, name: &str, bytes: &[u8]) -> bool {
        match download(name, bytes) {
            Ok(()) => true,
            Err(e) => {
                log::error!("couldn't download {name}: {e:?}");
                false
            }
        }
    }
    fn clipboard_set_text(&mut self, s: &str) {
        figcraft_ui_egui::egui_copy_text(&self.ctx, s);
    }
    fn clipboard_set_png(&mut self, w: u32, h: u32, rgba: &[u8]) {
        figcraft_ui_egui::egui_copy_png(&self.ctx, w, h, rgba);
    }
    fn open_url(&mut self, url: &str) {
        let Some(window) = web_sys::window() else { return };
        if let Err(e) = window.open_with_url_and_target_and_features(url, "_blank", "noopener,noreferrer") {
            log::error!("couldn't open {url}: {e:?}");
        }
    }
    fn reduce_motion(&self) -> bool {
        self.reduce_motion.as_ref().is_some_and(web_sys::MediaQueryList::matches)
    }
    /// The browser's language (`navigator.language`).
    fn system_language(&self) -> Option<String> {
        web_sys::window().and_then(|w| w.navigator().language())
    }
    /// The Gamepad API's controllers with the standard mapping (its button order is the layout's;
    /// others are skipped). Browsers list controllers once one of their buttons is pressed.
    fn gamepad_held(&mut self) -> u32 {
        let Some(pads) = web_sys::window().and_then(|w| w.navigator().get_gamepads().ok()) else { return 0 };
        let mut held = 0;
        for pad in pads.iter().filter_map(|p| p.dyn_into::<web_sys::Gamepad>().ok()) {
            if pad.mapping() != web_sys::GamepadMappingType::Standard {
                continue;
            }
            for (i, b) in pad.buttons().iter().take(32).enumerate() {
                if b.dyn_into::<web_sys::GamepadButton>().is_ok_and(|b| b.pressed()) {
                    held |= 1 << i;
                }
            }
        }
        held
    }
}

/// The file picked in the browser's file picker (`accept`: the extensions it offers), with its
/// name; `None` when the picker is cancelled, `Some(Err)` with what to tell the user when the file
/// can't be read.
async fn pick_file(accept: &str) -> Result<Option<Result<(String, Vec<u8>), String>>, wasm_bindgen::JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let body = document.body().ok_or("no body")?;
    let input: web_sys::HtmlInputElement = document.create_element("input")?.dyn_into()?;
    input.set_type("file");
    input.set_accept(accept);
    input.set_hidden(true);
    // Some browsers (older Safari) open the picker only for an input in the page.
    body.append_child(&input)?;
    let (closed, picker) = PickerClosed::new(&window, input.clone());
    input.click();
    let result = wasm_bindgen_futures::JsFuture::from(closed).await;
    let file = input.files().and_then(|files| files.get(0));
    drop(picker);
    result?;
    let Some(file) = file else { return Ok(None) };
    let name = file.name();
    if let Err(why) = figcraft_ui_egui::browser_can_read(&name, file.size()) {
        return Ok(Some(Err(why)));
    }
    Ok(Some(match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
        Ok(buffer) => Ok((name, js_sys::Uint8Array::new(&buffer).to_vec())),
        Err(e) => {
            log::error!("couldn't read {name}: {e:?}");
            Err(figcraft_ui_egui::unreadable(&name))
        }
    }))
}

/// How long after the page gets focus back the file picker counts as closed (milliseconds): a
/// picked file's `change` can come just after the focus, and must win. A `change` later than this
/// (a slow cloud-backed file in a browser that never sends `cancel`) counts as a cancel.
const PICKER_FOCUS_SETTLE_MS: i32 = 1_000;

/// Settles a file picker's promise once: on `change` (a file was picked), on `cancel` (closed
/// without one) or shortly after the page gets focus back, for browsers that never send `cancel`
/// (Safari before 16.4), where the pick would otherwise stay pending and block every later one.
/// Dropping it (also when the pick is abandoned) removes the input, the focus listener and its timer.
struct PickerClosed {
    window: web_sys::Window,
    input: web_sys::HtmlInputElement,
    _settle: wasm_bindgen::closure::Closure<dyn FnMut()>,
    on_focus: wasm_bindgen::closure::Closure<dyn FnMut()>,
    timer: std::rc::Rc<std::cell::Cell<Option<i32>>>,
}

impl PickerClosed {
    fn new(window: &web_sys::Window, input: web_sys::HtmlInputElement) -> (js_sys::Promise, Self) {
        let resolve = std::rc::Rc::new(std::cell::RefCell::new(None::<js_sys::Function>));
        let slot = resolve.clone();
        let closed = js_sys::Promise::new(&mut |res, _rej| *slot.borrow_mut() = Some(res));
        let settle = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(move || {
            if let Some(res) = resolve.borrow_mut().take() {
                let _ = res.call0(&wasm_bindgen::JsValue::NULL);
            }
        });
        let _ = input.add_event_listener_with_callback("change", settle.as_ref().unchecked_ref());
        let _ = input.add_event_listener_with_callback("cancel", settle.as_ref().unchecked_ref());
        let timer = std::rc::Rc::new(std::cell::Cell::new(None));
        let (w, t, f) = (window.clone(), timer.clone(), settle.as_ref().unchecked_ref::<js_sys::Function>().clone());
        let on_focus = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(move || {
            if t.get().is_none() {
                t.set(w.set_timeout_with_callback_and_timeout_and_arguments_0(&f, PICKER_FOCUS_SETTLE_MS).ok());
            }
        });
        let _ = window.add_event_listener_with_callback("focus", on_focus.as_ref().unchecked_ref());
        (closed, Self { window: window.clone(), input, _settle: settle, on_focus, timer })
    }
}

impl Drop for PickerClosed {
    fn drop(&mut self) {
        if let Some(t) = self.timer.take() {
            self.window.clear_timeout_with_handle(t);
        }
        let _ = self.window.remove_event_listener_with_callback("focus", self.on_focus.as_ref().unchecked_ref());
        self.input.remove();
    }
}

fn download(name: &str, bytes: &[u8]) -> Result<(), wasm_bindgen::JsValue> {
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let body = document.body().ok_or("no body")?;
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &{
        let o = web_sys::BlobPropertyBag::new();
        o.set_type("application/octet-stream");
        o
    })?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;
    let a: web_sys::HtmlAnchorElement = document.create_element("a")?.dyn_into()?;
    a.set_href(&url);
    a.set_download(name);
    body.append_child(&a)?;
    a.click();
    a.remove();
    // Revoked later, not at once: some browsers cancel a download whose URL goes before it starts.
    let revoke = wasm_bindgen::closure::Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    // Without the timer the URL just stays until the page closes.
    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), REVOKE_DOWNLOAD_MS);
    Ok(())
}

/// How long a download's object URL is kept (milliseconds).
const REVOKE_DOWNLOAD_MS: i32 = 40_000;
