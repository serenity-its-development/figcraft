//! The live-editing shim and the assistant's command surface for node: one [`Window`] is one editor window's side of the host
//! bridge, with the design held as the engine's session, so a test (OneOS's `craft-design-convergence.test.ts`) can drive two
//! windows through the host's real adapter and CRDT engine and compare what they end up showing, and drive a design through the
//! same `command.batch` the browser build answers (`craft-design-ai-drive.test.ts`). Everything the browser build shares is here:
//! the same `HostLive` glue and the same `Session::run_command_batch`.

use base64::Engine as _;
use figcraft_engine::Session;
use figcraft_engine::host_live::HostLive;
use figcraft_host_collab::{Batch, changes_of, digest};
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

fn err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

fn batches_json(batches: &[Batch]) -> String {
    Value::Array(batches.iter().map(|b| json!({"seq": b.seq, "ops": b.ops, "label": b.label, "undo": if b.join { "join" } else { "step" }, "seed": b.seed})).collect())
        .to_string()
}

#[wasm_bindgen]
pub struct Window {
    session: Session,
    live: HostLive,
}

#[wasm_bindgen]
impl Window {
    /// A window showing a new design, or the `.figcraft` file in `file_b64` (base64); its layer ids drawn from the space of `client`.
    #[wasm_bindgen(constructor)]
    pub fn new(client: u32, file_b64: &str) -> Result<Window, JsError> {
        let mut session = if file_b64.is_empty() { Session::new() } else { Session::empty() };
        if !file_b64.is_empty() {
            session.execute("file.open", &json!({"data": file_b64, "name": "design.figcraft"})).map_err(err)?;
        }
        let live = HostLive::new(&mut session, client).map_err(err)?;
        Ok(Window { session, live })
    }

    /// The design as a `.figcraft` file (base64): a second window opens the same file by it.
    pub fn file_b64(&mut self) -> Result<String, JsError> {
        let ds = self.session.ds().map_err(err)?;
        let bytes = figcraft_engine::cmd::file::file_bytes(ds).map_err(err)?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    /// Run one registry command as the person does (a menu, a shortcut): `params_json` its parameters; answers its result as JSON.
    pub fn run(&mut self, command: &str, params_json: &str) -> Result<String, JsError> {
        let params: Value = serde_json::from_str(params_json).map_err(|e| JsError::new(&format!("params: {e}")))?;
        self.session.execute(command, &params).map(|v| v.to_string()).map_err(err)
    }

    /// The assistant's run: what the browser build answers a `craft:command {method: "command.batch"}` with. A refusal is the
    /// thrown error's message.
    pub fn command_batch(&mut self, params_json: &str) -> Result<String, JsError> {
        let params: Value = serde_json::from_str(params_json).map_err(|e| JsError::new(&format!("params: {e}")))?;
        self.session.run_command_batch(&params).map(|v| v.to_string()).map_err(|e| JsError::new(&e))
    }

    /// The seed batches for the opened design (an array of `{seq, ops, label, undo, seed}`).
    pub fn seed(&mut self, now: f64) -> Result<String, JsError> {
        Ok(batches_json(&self.live.seed(&mut self.session, now as u64).map_err(err)?))
    }

    /// The person edited the design: answers the batches to send.
    pub fn flush(&mut self, now: f64) -> Result<String, JsError> {
        let label = self.session.undo_labels(1).into_iter().next().unwrap_or_else(|| "Edit".to_string());
        Ok(batches_json(&self.live.flush(&self.session, &label, false, now as u64).map_err(err)?))
    }

    /// The host sent `craft:crdt:state`: `doc_json` is its `doc`, `comments_json` its `comments` (unused: comments are entries).
    pub fn state(&mut self, doc_json: &str, _comments_json: &str) -> Result<(), JsError> {
        let doc: Value = serde_json::from_str(doc_json).map_err(|e| JsError::new(&format!("state: {e}")))?;
        self.live.state(&mut self.session, &doc, 0).map_err(err)?;
        Ok(())
    }

    /// The host sent `craft:crdt:delta`: `changes_json` is its `changes` (design paths start with `doc`); answers whether the design
    /// changed.
    pub fn delta(&mut self, changes_json: &str, ack: f64, now: f64) -> Result<bool, JsError> {
        let all: Value = serde_json::from_str(changes_json).map_err(|e| JsError::new(&format!("changes: {e}")))?;
        let changes: Vec<_> = changes_of(&all)
            .into_iter()
            .filter_map(|mut c| {
                (c.path.first().map(String::as_str) == Some("doc")).then(|| {
                    c.path.remove(0);
                    c
                })
            })
            .collect();
        self.live.delta(&mut self.session, &changes, ack as u64, now as u64).map_err(err)
    }

    /// Comments are entries of the shared tree here, not the host's comments tier: nothing to send.
    pub fn comment_msgs(&mut self) -> String {
        "[]".to_string()
    }

    /// The Undo and Redo requests the person made since the last call (`true`: undo), as JSON: the room does them.
    pub fn take_undo_requests(&mut self) -> String {
        json!(std::mem::take(&mut self.session.collab_undo_requests)).to_string()
    }

    /// What the design shows of what the shim shares, as one comparable JSON string.
    pub fn digest(&self) -> Result<String, JsError> {
        let ds = self.session.ds().map_err(err)?;
        Ok(digest(&ds.doc).map_err(err)?.to_string())
    }

    /// Changes the shim could not apply (0 in a healthy room).
    pub fn skipped(&self) -> u32 {
        self.live.skipped() as u32
    }
}
