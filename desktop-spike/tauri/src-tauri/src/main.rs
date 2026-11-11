//! D1 evidence spike: one window over a static page. It appends timing lines
//! to the file named by `SPIKE_REPORT` (stdout is detached on Windows) and
//! exits after `SPIKE_EXIT_AFTER_SECS`, so a runner can measure it unattended.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::Write as _;
use std::time::{Duration, Instant};

fn note(line: &str) {
    if let Some(path) = std::env::var_os("SPIKE_REPORT")
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

fn main() {
    let started = Instant::now();
    let exit_after = std::env::var("SPIKE_EXIT_AFTER_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok());
    let result = tauri::Builder::default()
        .on_page_load(move |_webview, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                note(&format!(
                    "SPIKE_PAGE_LOADED_MS {}",
                    started.elapsed().as_millis()
                ));
            }
        })
        .setup(move |app| {
            if let Some(seconds) = exit_after {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(seconds));
                    handle.exit(0);
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!());
    if let Err(error) = result {
        note(&format!("SPIKE_ERROR {error}"));
        std::process::exit(1);
    }
}
