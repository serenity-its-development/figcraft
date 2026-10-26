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

use std::collections::BTreeMap;

use figcraft_engine::Session;
use figcraft_engine::host_live::HostLive;
use figcraft_host_collab::{Batch, changes_of};
use serde_json::{Value, json};

use figcraft_ui_egui::FigApp;

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

/// Another person in the room: their name, colour and last cursor body (resolved against the design every frame).
struct Peer {
    name: String,
    color: [u8; 3],
    cursor: Value,
}

/// My pointer and selection go out when they move (at most this often) and again this often while they stay, so the others' copy of
/// them does not expire (the host drops a cursor 6 s after its last frame).
const CURSOR_MIN_MS: u64 = 80;
const CURSOR_REPEAT_MS: u64 = 3_000;

/// `#rrggbb` as colour bytes.
fn rgb_of(s: &str) -> [u8; 3] {
    let h = s.trim_start_matches('#');
    let byte = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).unwrap_or(0x7c);
    if h.len() == 6 { [byte(0), byte(2), byte(4)] } else { [0x7c, 0x3a, 0xed] }
}

pub struct Collab {
    bridge: Bridge,
    live: Option<HostLive>,
    phase: Phase,
    /// The last flush was part of a drag or a typing burst: the next edit folds into the same undo step.
    gesture: bool,
    peers: BTreeMap<String, Peer>,
    last_cursor: Option<Value>,
    last_cursor_ms: u64,
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
        Collab { bridge, live: None, phase: Phase::Off, gesture: false, peers: BTreeMap::new(), last_cursor: None, last_cursor_ms: 0 }
    }

    /// A `craft:open` arrived and the file is open in the session as its active document.
    pub fn opened(&mut self, o: &Opened, session: &mut Session) {
        self.live = None;
        self.gesture = false;
        self.peers.clear();
        self.last_cursor = None;
        session.room_peers.clear();
        session.room_pointer = None;
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

    fn off(&mut self, app: &mut FigApp, why: &str) {
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
    fn flush_local(&mut self, app: &mut FigApp) {
        if self.phase != Phase::Live {
            return;
        }
        let Some(live) = self.live.as_mut() else { return };
        if self.bridge.opened().read_only {
            return;
        }
        let in_gesture = app.session.docs.iter().any(|d| d.interaction.is_some() || d.text_edit.is_some());
        let label = app.session.undo_labels(1).into_iter().next().unwrap_or_else(|| "Edit".to_string());
        match live.flush(&mut app.session, &label, self.gesture, now_ms()) {
            Ok(batches) => {
                self.gesture = in_gesture;
                self.post_batches(&batches);
            }
            Err(e) => self.off(app, &e),
        }
    }

    /// The host sent something for the live channel.
    pub fn event(&mut self, ev: Event, app: &mut FigApp) {
        match ev {
            Event::Refused { code, reason } => app.toast(if code.is_empty() { reason } else { format!("{reason} ({code})") }),
            Event::Crdt { kind, body } => self.handle(app, &kind, &body),
            _ => {}
        }
    }

    fn handle(&mut self, app: &mut FigApp, kind: &str, body: &Value) {
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
            "peer" => {
                let Some(party) = body.get("party").and_then(Value::as_str) else { return };
                match body.get("cursor") {
                    Some(c) if !c.is_null() => {
                        let name = body.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).unwrap_or("Someone").to_owned();
                        let color = rgb_of(body.get("color").and_then(Value::as_str).unwrap_or("#7c3aed"));
                        self.peers.insert(party.to_owned(), Peer { name, color, cursor: c.clone() });
                    }
                    _ => {
                        self.peers.remove(party);
                    }
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
    fn post_held(&mut self, app: &mut FigApp) {
        self.flush_local(app);
    }

    /// Once per frame AFTER the app's own pass.
    pub fn post(&mut self, app: &mut FigApp) {
        self.flush_local(app);
        for undo in std::mem::take(&mut app.session.collab_undo_requests) {
            self.bridge.post_crdt(if undo { "undo" } else { "redo" }, &json!({}));
        }
        if self.phase == Phase::Live {
            self.share_cursor(app);
            self.show_peers(app);
        }
    }

    /// Say where my pointer and selection are: when they move, and now and then while they stay.
    fn share_cursor(&mut self, app: &FigApp) {
        let now = now_ms();
        let Some(body) = self.live.as_ref().and_then(|l| l.cursor_body(&app.session)) else { return };
        let moved = self.last_cursor.as_ref() != Some(&body);
        let due = if moved { now.saturating_sub(self.last_cursor_ms) >= CURSOR_MIN_MS } else { now.saturating_sub(self.last_cursor_ms) >= CURSOR_REPEAT_MS };
        if !due {
            return;
        }
        self.bridge.post_crdt("cursor", &json!({"cursor": body}));
        self.last_cursor = Some(body);
        self.last_cursor_ms = now;
    }

    /// The other people, resolved against the design as it is now.
    fn show_peers(&mut self, app: &mut FigApp) {
        let Some(live) = self.live.as_ref() else { return };
        let peers: Vec<_> = self.peers.values().filter_map(|p| live.peer(&app.session, &p.name, p.color, &p.cursor)).collect();
        if peers != app.session.room_peers {
            app.session.room_peers = peers;
        }
    }
}
