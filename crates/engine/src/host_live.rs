//! Live editing through the OneOS host: the glue between a [`Session`]'s document and [`figcraft_host_collab::Shim`], used by the
//! browser build (`figcraft-web`'s `collab.rs`) and by the node harness OneOS's convergence test drives (`collab-node`), so the test
//! runs the code the browser runs.
//!
//! Per frame the caller does, in this order: [`HostLive::flush`] (the person's own edits since the last frame go out FIRST, so none
//! is overwritten), then the host's `state` / `delta` (shown in the session without an undo step), then the frame's own work, then
//! [`HostLive::flush`] again. Undo and Redo are the room's: while a document is live the session queues them
//! ([`Session::collab_undo_requests`]) and the caller sends `craft:crdt:undo` / `redo`.

use std::sync::Arc;

use figcraft_host_collab::{Batch, HostChange, Shim};
use serde_json::Value;

use crate::Session;

/// One window's live document.
pub struct HostLive {
    shim: Shim,
    /// The document (`DocState::uid`) that is shared.
    uid: u64,
    /// Batches a flush before a remote change made that the caller has not taken yet.
    held: Vec<Batch>,
}

impl HostLive {
    /// Share the session's active document; layer ids this window makes are in the space of `client` (not 0, and different in every
    /// window of the room).
    pub fn new(session: &mut Session, client: u32) -> Result<HostLive, String> {
        let ds = session.ds_mut().map_err(|e| e.to_string())?;
        let uid = ds.uid;
        ds.doc_mut().set_client_id(client);
        Ok(HostLive { shim: Shim::new(client), uid, held: Vec::new() })
    }

    /// The document is no longer live (the room ended): Undo is the session's again.
    pub fn end(self, session: &mut Session) {
        session.collab_undo = false;
        session.collab_undo_requests.clear();
    }

    /// The shared document's tab.
    #[must_use]
    pub fn uid(&self) -> u64 {
        self.uid
    }

    /// Changes the shim could not apply (0 in a healthy room).
    #[must_use]
    pub fn skipped(&self) -> usize {
        self.shim.skipped
    }

    fn doc(&self, session: &Session) -> Result<Arc<figcraft_doc::Document>, String> {
        session.docs.iter().find(|d| d.uid == self.uid).map(|d| d.doc.clone()).ok_or_else(|| "the shared document was closed".to_string())
    }

    /// The room is empty: the document as ops (the first batch is the seed).
    pub fn seed(&mut self, session: &mut Session, now_ms: u64) -> Result<Vec<Batch>, String> {
        let doc = self.doc(session)?;
        let batches = self.shim.seed(&doc, now_ms).map_err(|e| e.to_string())?;
        session.collab_undo = true;
        Ok(batches)
    }

    /// The person's edits since the last call as batches to send (`join`: a gesture or typing burst continues, so the edit folds
    /// into the previous undo step); includes any a remote change made go out first.
    pub fn flush(&mut self, session: &Session, label: &str, join: bool, now_ms: u64) -> Result<Vec<Batch>, String> {
        let doc = self.doc(session)?;
        let mut out = std::mem::take(&mut self.held);
        out.extend(self.shim.local(&doc, label, join, now_ms).map_err(|e| e.to_string())?);
        Ok(out)
    }

    /// The whole shared state arrived: show it. Answers whether the document changed.
    pub fn state(&mut self, session: &mut Session, craft: &Value, now_ms: u64) -> Result<bool, String> {
        self.send_own_first(session, now_ms)?;
        let doc = self.doc(session)?;
        let changed = self.shim.remote_state(craft, &doc).map_err(|e| e.to_string())?;
        self.install(session);
        session.collab_undo = true;
        Ok(changed)
    }

    /// The host reported changes (paths below the `craft` root) and `ack`, the last of this window's batches it applied.
    pub fn delta(&mut self, session: &mut Session, changes: &[HostChange], ack: u64, now_ms: u64) -> Result<bool, String> {
        self.send_own_first(session, now_ms)?;
        let doc = self.doc(session)?;
        let changed = self.shim.remote_delta(changes, ack, now_ms, &doc).map_err(|e| e.to_string())?;
        if changed {
            self.install(session);
        }
        Ok(changed)
    }

    /// Edits made since the last flush are sent before a remote change is shown; the batches wait in `held` for the caller.
    fn send_own_first(&mut self, session: &Session, now_ms: u64) -> Result<(), String> {
        let doc = self.doc(session)?;
        let batches = self.shim.local(&doc, "Edit", false, now_ms).map_err(|e| e.to_string())?;
        self.held.extend(batches);
        Ok(())
    }

    /// Show the shared document in the session: others' edits arrive without an undo step, and the selection and page stay where
    /// they can.
    fn install(&self, session: &mut Session) {
        let Some(shared) = self.shim.document() else { return };
        let mut doc = shared.clone();
        if let Some(ds) = session.docs.iter().find(|d| d.uid == self.uid) {
            // The library the file publishes is not shared.
            doc.published = ds.doc.published.clone();
        }
        session.apply_remote(self.uid, Arc::new(doc));
    }
}
