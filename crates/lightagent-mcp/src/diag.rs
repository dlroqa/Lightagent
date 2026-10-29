//! DIAGNOSTIC ONLY (branch diag/windows-stall, never merged): a timeline of
//! stdio server lifecycle events for issue #82, appended to the file that
//! `LIGHTAGENT_DIAG_TIMELINE` names. Event names, process ids, request ids
//! and times only: never commands, arguments, environments or contents.

use std::io::Write as _;
use std::sync::{LazyLock, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static START: LazyLock<Instant> = LazyLock::new(Instant::now);
static SINK: LazyLock<Option<Mutex<std::fs::File>>> = LazyLock::new(|| {
    let path = std::env::var_os("LIGHTAGENT_DIAG_TIMELINE")?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
        .map(Mutex::new)
});

/// Append one event, when a timeline file is configured.
pub fn event(what: &str) {
    let Some(sink) = SINK.as_ref() else {
        return;
    };
    let mono = START.elapsed().as_micros();
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let thread = std::thread::current();
    let line = format!(
        "{wall} pid={} mono_us={mono} thread={} {what}\n",
        std::process::id(),
        thread.name().unwrap_or("?")
    );
    if let Ok(mut file) = sink.lock() {
        let _ = file.write_all(line.as_bytes());
    }
}
