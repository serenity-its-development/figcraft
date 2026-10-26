//! Where a file goes in the browser build when OneOS frames it. The page has no file system; the host bridge installs a sink that
//! takes the bytes of every write a command makes (Save, Save as, Export) and hands them to the host, which makes a new version of
//! the opened file (Save) or a copy (anything under another name). With no sink installed a write is refused, as before.

use std::cell::RefCell;

use crate::{Result, Session};

/// Takes `(name, bytes)`; `Err` says why the host could not take it.
pub type Sink = Box<dyn Fn(&str, &[u8]) -> std::result::Result<(), String>>;

thread_local! {
    static SINK: RefCell<Option<Sink>> = const { RefCell::new(None) };
}

/// Install (or, with `None`, remove) the sink writes go to in the browser build.
pub fn set_write_sink(sink: Option<Sink>) {
    SINK.with(|s| *s.borrow_mut() = sink);
}

/// Hand `bytes` to the sink: `None` when there is none.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn offer(name: &str, bytes: &[u8]) -> Option<std::result::Result<(), String>> {
    SINK.with(|s| s.borrow().as_ref().map(|sink| sink(name, bytes)))
}

impl Session {
    /// The document `uid` (the last one opened when `None`) is the host's file `name`: Save writes under that name, so the host
    /// makes a new version of it.
    pub fn bind_host_name(&mut self, uid: Option<u64>, name: &str) -> Result<()> {
        let ds = match uid {
            Some(u) => self.docs.iter_mut().find(|d| d.uid == u),
            None => self.docs.last_mut(),
        }
        .ok_or(crate::EngineError::NoDocument)?;
        ds.path = Some(name.to_string());
        Ok(())
    }

    /// Whether any document has changes the host does not hold yet.
    pub fn host_dirty(&self) -> bool {
        self.docs.iter().any(crate::DocState::is_dirty)
    }
}
