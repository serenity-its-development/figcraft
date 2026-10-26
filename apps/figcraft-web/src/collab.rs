//! Live editing for the host bridge: the frame side of the host's common collaboration adapter (design section 12). The shim
//! itself is `figcraft-host-collab` (pure, tested natively and against the host's real engine) and the glue to the session is
//! `figcraft_engine::host_live` (the code OneOS's convergence test runs too); this module is its wiring: it turns the session's
//! edits into `craft:crdt:ops`, and the host's `state` / `delta` / `seed` / `export` into the design the editor shows.
//!
//! FigCraft's own multiplayer (its server, links as access control) is not used. What goes both ways: layers and their
//! properties, the file's own properties, styles, variables and comment threads. Undo and Redo are the room's
//! (`craft:crdt:undo` / `redo`): the session's own snapshots would also take back what others did.
//!
//! Order each frame: the host's messages first (before a remote change is shown, the person's own edits since the last frame go
//! out, so none is overwritten); then the app's own pass; then any edit this frame made is sent.

use figcraft_engine::Session;
use figcraft_engine::host_live::HostLive;
use figcraft_host_collab::{Batch, changes_of};
use serde_json::{Value, json};

use crate::host::{Bridge, Event, Opened};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Not a live document (the host opened it without `collab`, or live editing ended).
    Off,
    /// Opened with `collab`: waiting for the host to seed the room or send its state.
    Joining,
    /// In the room.
    Live,
}

pub struct Collab {
    bridge: Bridge,
    live: Option<HostLive>,
    phase: Phase,
    /// The last flush was part of a drag or a typing burst: the next edit folds into the same undo step.
    gesture: bool,
}

fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

/// A client id for layer ids: random, never 0.
fn random_client() -> u32 {
    let mut buf = [0u8; 4];
    if let Some(crypto) = web_sys::window().and_then(|w| w.crypto().ok()) {
        let _ = crypto.get_random_values_with_u8_array(&mut buf);
    }
    (u32::from_le_bytes(buf) ^ (js_sys::Math::random() * 4_294_967_295.0) as u32).max(1)
}

impl Collab {
    pub fn new(bridge: Bridge) -> Self {
        Collab { bridge, live: None, phase: Phase::Off, gesture: false }
    }

    /// A `craft:open` arrived and the file is open in the session as its active document.
    pub fn opened(&mut self, o: &Opened, session: &mut Session) {
        self.live = None;
        self.gesture = false;
        session.collab_undo = false;
        session.collab_undo_requests.clear();
        self.phase = Phase::Off;
        if !o.collab {
            return;
        }
        match HostLive::new(session, random_client()) {
            Ok(live) => {
                self.live = Some(live);
                self.phase = Phase::Joining;
            }
            Err(e) => log::error!("live editing could not start: {e}"),
        }
    }

    fn off(&mut self, app: &mut figcraft_ui_egui::FigApp, why: &str) {
        self.phase = Phase::Off;
        if let Some(live) = self.live.take() {
            live.end(&mut app.session);
        }
        app.toast(format!("Live editing is off: {why}"));
    }

    fn post_batches(&self, batches: &[Batch]) {
        for b in batches {
            let mut body = json!({"ops": b.ops, "seq": b.seq, "undo": if b.join { "join" } else { "step" }, "label": b.label});
            if b.seed {
                body["seed"] = json!(true);
            }
            self.bridge.post_crdt("ops", &body);
        }
    }

    /// Send what the person changed since the last frame.
    fn flush_local(&mut self, app: &mut figcraft_ui_egui::FigApp) {
        if self.phase != Phase::Live {
            return;
        }
        let Some(live) = self.live.as_mut() else { return };
        if self.bridge.opened().read_only {
            return;
        }
        let in_gesture = app.session.docs.iter().any(|d| d.interaction.is_some() || d.text_edit.is_some());
        let label = app.session.undo_labels(1).into_iter().next().unwrap_or_else(|| "Edit".to_string());
        match live.flush(&app.session, &label, self.gesture, now_ms()) {
            Ok(batches) => {
                self.gesture = in_gesture;
                self.post_batches(&batches);
            }
            Err(e) => self.off(app, &e),
        }
    }

    /// The host sent something for the live channel.
    pub fn event(&mut self, ev: Event, app: &mut figcraft_ui_egui::FigApp) {
        match ev {
            Event::Refused { code, reason } => app.toast(if code.is_empty() { reason } else { format!("{reason} ({code})") }),
            Event::Crdt { kind, body } => self.handle(app, &kind, &body),
            _ => {}
        }
    }

    fn handle(&mut self, app: &mut figcraft_ui_egui::FigApp, kind: &str, body: &Value) {
        if self.live.is_none() {
            return;
        }
        match kind {
            "state" => {
                let doc = body.get("state").and_then(|s| s.get("doc")).cloned().unwrap_or(Value::Null);
                // An empty room has nothing to show: the host asks for the seed.
                if doc.as_object().is_none_or(|o| o.get("n").is_none()) {
                    return;
                }
                self.flush_local(app);
                let Some(live) = self.live.as_mut() else { return };
                match live.state(&mut app.session, &doc, now_ms()) {
                    Ok(_) => {
                        self.phase = Phase::Live;
                        self.post_held(app);
                    }
                    Err(e) => self.off(app, &e),
                }
            }
            "seed" => {
                let Some(live) = self.live.as_mut() else { return };
                match live.seed(&mut app.session, now_ms()) {
                    Ok(batches) => {
                        self.phase = Phase::Live;
                        self.post_batches(&batches);
                    }
                    Err(e) => self.off(app, &e),
                }
            }
            "delta" => {
                if self.phase != Phase::Live {
                    return;
                }
                let ack = body.get("ack").and_then(Value::as_u64).unwrap_or(0);
                // The shim reads what is below `doc`.
                let changes: Vec<_> = changes_of(body.get("changes").unwrap_or(&Value::Null))
                    .into_iter()
                    .filter_map(|mut c| {
                        (c.path.first().map(String::as_str) == Some("doc")).then(|| {
                            c.path.remove(0);
                            c
                        })
                    })
                    .collect();
                let Some(live) = self.live.as_mut() else { return };
                if let Err(e) = live.delta(&mut app.session, &changes, ack, now_ms()) {
                    self.off(app, &e);
                    return;
                }
                self.post_held(app);
            }
            "export" => {
                self.flush_local(app);
                let request = body.get("request").and_then(Value::as_u64);
                let opened = self.bridge.opened();
                let Some(ds) = app.session.docs.iter().find(|d| d.uid == self.uid()) else { return };
                match figcraft_engine::cmd::file::file_bytes(ds) {
                    Ok(bytes) => {
                        if let Err(e) = self.bridge.post_save(&opened.name, &bytes, false, request) {
                            app.toast(format!("The checkpoint was not sent: {e}"));
                        }
                    }
                    Err(e) => app.toast(format!("The checkpoint could not be made: {e}")),
                }
            }
            "unplaced" => app.toast("Some of your offline edits could not be placed in the live design."),
            _ => {}
        }
    }

    fn uid(&self) -> u64 {
        self.live.as_ref().map_or(0, HostLive::uid)
    }

    /// Edits a remote change made go out first, so they are posted right after the change.
    fn post_held(&mut self, app: &mut figcraft_ui_egui::FigApp) {
        self.flush_local(app);
    }

    /// Once per frame AFTER the app's own pass.
    pub fn post(&mut self, app: &mut figcraft_ui_egui::FigApp) {
        self.flush_local(app);
        for undo in std::mem::take(&mut app.session.collab_undo_requests) {
            self.bridge.post_crdt(if undo { "undo" } else { "redo" }, &json!({}));
        }
    }
}
