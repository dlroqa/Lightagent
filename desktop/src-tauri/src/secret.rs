//! Host-side secret input (D8): the desktop's `lightagent remote-credential
//! add|rotate --secret-stdin`.
//!
//! The window never holds the secret. It asks the host to add or rotate a
//! grant with the non-secret fields it has. The host opens the platform's
//! own file picker (GTK on Linux, NSOpenPanel on macOS, the common item
//! dialog on Windows), reads the chosen file itself, and sends the secret
//! to its runtime in the request body, as the CLI pipes it on standard
//! input. The window gets back what happened, the file's name and the
//! runtime's answer, which carries no secret. A request from the window
//! that names a secret itself is refused (`host_only_body` in the
//! contract).
//!
//! Nothing is written. The chosen file is left exactly as it was: deleting
//! it is the person's choice. The host keeps no copy once the request is
//! sent; the secret is stored only as the vault seals it.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde::Serialize;
use serde_json::Value;

use crate::Host;

/// The most a secret file may hold, as the CLI reads from standard input.
const MAX_FILE: u64 = 16 * 1024;
/// The vault's own limit on a secret (lightagent_core::credentials).
const MAX_SECRET: usize = 8 * 1024;
// Windows passes the same title as a wide literal (w!), so it reads none.
#[cfg_attr(windows, allow(dead_code))]
const TITLE: &str = "Choose the file that holds the secret";

/// What happened. Never carries the secret.
#[derive(Debug, Serialize)]
pub struct Outcome {
    /// `stored`; `cancelled` (no file chosen, nothing sent); `invalid` (the
    /// file cannot be used, nothing sent); `refused` (the runtime said no).
    pub state: &'static str,
    /// The chosen file's name only, never its path or contents.
    pub file_name: Option<String>,
    pub status: Option<u16>,
    /// The runtime's answer: grant metadata or its error.
    pub body: Option<String>,
    pub reason: Option<String>,
}

impl Outcome {
    fn new(state: &'static str, file_name: Option<String>) -> Self {
        Self {
            state,
            file_name,
            status: None,
            body: None,
            reason: None,
        }
    }
}

/// Add or rotate (`target`, a contract operation that takes a host-only
/// secret) with `fields` from the window and a secret the host reads.
pub async fn submit(
    host: &Host,
    app: &tauri::AppHandle,
    target: String,
    fields: String,
) -> Result<Outcome, String> {
    let operation = host.contract.resolve("POST", &target)?.clone();
    if !operation
        .host_only_body
        .iter()
        .any(|field| field == "secret")
    {
        return Err(format!("{} takes no secret from the host", operation.id));
    }
    if host.switching.load(Ordering::Acquire)
        && crate::switch::blocked_while_switching(&operation.id)
    {
        return Err("a profile switch is in progress; try again when it has finished".to_owned());
    }
    let Ok(Value::Object(mut fields)) = serde_json::from_str::<Value>(&fields) else {
        return Err("the request fields must be a JSON object".to_owned());
    };
    if fields.contains_key("secret") || fields.contains_key("secret_file") {
        return Err(
            "the window names no secret and no secret file: the host asks for the file".to_owned(),
        );
    }
    let Some(path) = pick(app).await? else {
        return Ok(Outcome::new("cancelled", None));
    };
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let secret = match read_secret(&path) {
        Ok(secret) => secret,
        Err(reason) => {
            return Ok(Outcome {
                reason: Some(reason),
                ..Outcome::new("invalid", file_name)
            });
        }
    };
    fields.insert("secret".to_owned(), Value::String(secret));
    let body = Value::Object(fields).to_string();
    let answer = crate::forward(host, &operation, "POST", &target, Some(body), None, None).await?;
    let state = if (200..300).contains(&answer.status) {
        "stored"
    } else {
        "refused"
    };
    Ok(Outcome {
        status: Some(answer.status),
        body: Some(answer.body),
        ..Outcome::new(state, file_name)
    })
}

/// The secret in `path`, checked as the vault checks it, or why it cannot
/// be used. A reason never quotes the file.
fn read_secret(path: &Path) -> Result<String, String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("the chosen file could not be opened ({})", error.kind()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("the chosen file could not be read ({})", error.kind()))?;
    if !metadata.is_file() {
        return Err("the chosen item is not a regular file".to_owned());
    }
    let too_large = || {
        format!(
            "the chosen file is over {} KiB; a secret file holds only the secret",
            MAX_FILE / 1024
        )
    };
    if metadata.len() > MAX_FILE {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    if let Err(error) = file.take(MAX_FILE + 1).read_to_end(&mut bytes) {
        wipe(bytes);
        return Err(format!(
            "the chosen file could not be read ({})",
            error.kind()
        ));
    }
    if bytes.len() as u64 > MAX_FILE {
        wipe(bytes);
        return Err(too_large());
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            wipe(error.into_bytes());
            return Err("the chosen file is not text (UTF-8)".to_owned());
        }
    };
    let problem = {
        let value = text.trim();
        if value.is_empty() {
            Some("the chosen file is empty")
        } else if value.contains(['\r', '\n', '\0']) {
            Some("a secret is one line, and the chosen file has more than one")
        } else if value.len() > MAX_SECRET {
            Some("the secret is over 8 KiB")
        } else {
            None
        }
    };
    if let Some(problem) = problem {
        wipe(text.into_bytes());
        return Err(problem.to_owned());
    }
    Ok(text)
}

/// Overwrite a buffer that held secret bytes before it is released.
fn wipe(mut bytes: Vec<u8>) {
    bytes.fill(0);
    std::hint::black_box(&bytes);
}

async fn pick(app: &tauri::AppHandle) -> Result<Option<PathBuf>, String> {
    // A test build may answer for the picker (e2e_driver.rs, `pick_file`).
    #[cfg(feature = "e2e-hooks")]
    if let Some(choice) = crate::e2e_driver::chosen_file() {
        return Ok(choice);
    }
    native::pick(app).await
}

#[cfg(target_os = "linux")]
mod native {
    use std::path::PathBuf;

    use tauri::Manager as _;

    pub async fn pick(app: &tauri::AppHandle) -> Result<Option<PathBuf>, String> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let window = app.get_webview_window("main");
        app.run_on_main_thread(move || {
            use gtk::prelude::*;
            let parent = window.and_then(|window| window.gtk_window().ok());
            let dialog = gtk::FileChooserNative::new(
                Some(super::TITLE),
                parent.as_ref(),
                gtk::FileChooserAction::Open,
                Some("Use this file"),
                Some("Cancel"),
            );
            dialog.set_modal(true);
            let chosen = if dialog.run() == gtk::ResponseType::Accept {
                dialog.filename()
            } else {
                None
            };
            dialog.destroy();
            let _ = sender.send(chosen);
        })
        .map_err(|error| format!("the file picker could not open: {error}"))?;
        receiver
            .await
            .map_err(|_| "the file picker closed without an answer".to_owned())
    }
}

#[cfg(target_os = "macos")]
mod native {
    use std::path::PathBuf;

    pub async fn pick(app: &tauri::AppHandle) -> Result<Option<PathBuf>, String> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        app.run_on_main_thread(move || {
            let _ = sender.send(open_panel());
        })
        .map_err(|error| format!("the file picker could not open: {error}"))?;
        receiver
            .await
            .map_err(|_| "the file picker closed without an answer".to_owned())
    }

    #[allow(unused_unsafe)]
    fn open_panel() -> Option<PathBuf> {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
        use objc2_foundation::NSString;

        let mtm = MainThreadMarker::new()?;
        // SAFETY: on the main thread (checked above); the panel is created,
        // run modally and released here.
        unsafe {
            let panel = NSOpenPanel::openPanel(mtm);
            panel.setCanChooseFiles(true);
            panel.setCanChooseDirectories(false);
            panel.setAllowsMultipleSelection(false);
            panel.setMessage(Some(&NSString::from_str(super::TITLE)));
            if panel.runModal() != NSModalResponseOK {
                return None;
            }
            let url = panel.URL()?;
            let path = url.path()?;
            Some(PathBuf::from(path.to_string()))
        }
    }
}

#[cfg(windows)]
mod native {
    use std::path::PathBuf;

    pub async fn pick(_app: &tauri::AppHandle) -> Result<Option<PathBuf>, String> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        // The dialog runs on its own apartment-threaded thread.
        std::thread::spawn(move || {
            let _ = sender.send(dialog());
        });
        receiver
            .await
            .map_err(|_| "the file picker closed without an answer".to_owned())?
    }

    #[allow(unused_unsafe)]
    fn dialog() -> Result<Option<PathBuf>, String> {
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoTaskMemFree, CoUninitialize,
        };
        use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
        use windows::core::w;

        /// HRESULT_FROM_WIN32(ERROR_CANCELLED): the person closed the dialog.
        const CANCELLED: u32 = 0x8007_04C7;
        // SAFETY: COM is initialised on this thread for the dialog's
        // lifetime, and the returned path's memory is freed once copied.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                .ok()
                .map_err(|error| format!("the file picker could not start: {}", error.message()))?;
            let result = (|| {
                let dialog: IFileOpenDialog =
                    CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(
                        |error| format!("the file picker could not open: {}", error.message()),
                    )?;
                let _ = dialog.SetTitle(w!("Choose the file that holds the secret"));
                if let Err(error) = dialog.Show(None) {
                    if error.code().0 as u32 == CANCELLED {
                        return Ok(None);
                    }
                    return Err(format!("the file picker failed: {}", error.message()));
                }
                let item = dialog.GetResult().map_err(|error| {
                    format!("the file picker returned nothing: {}", error.message())
                })?;
                let name = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(|error| {
                    format!("the chosen item has no file path: {}", error.message())
                })?;
                let path = name.to_string();
                CoTaskMemFree(Some(name.0 as *const _));
                path.map(|path| Some(PathBuf::from(path)))
                    .map_err(|_| "the chosen path is not valid Unicode".to_owned())
            })();
            CoUninitialize();
            result
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod native {
    pub async fn pick(_app: &tauri::AppHandle) -> Result<Option<std::path::PathBuf>, String> {
        let _ = super::TITLE;
        Err("this platform has no native file picker here".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lightagent-desktop-secret-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("file");
        path
    }

    #[test]
    fn a_secret_file_is_read_and_checked_without_quoting_it() {
        let good = file("good", b"  host-read-secret-1234  \n");
        assert_eq!(
            read_secret(&good).expect("good").trim(),
            "host-read-secret-1234"
        );
        let cases: [(&str, &[u8], &str); 4] = [
            ("empty", b" \n", "empty"),
            (
                "lines",
                b"first-secret-line\nsecond-secret-line\n",
                "one line",
            ),
            ("binary", b"\xff\xfe\x00secret", "not text"),
            ("large", &[b'a'; 17 * 1024], "over 16 KiB"),
        ];
        for (name, bytes, said) in cases {
            let reason = read_secret(&file(name, bytes)).expect_err(name);
            assert!(reason.contains(said), "{name}: {reason}");
            assert!(
                !reason.contains("secret-line") && !reason.contains("aaaa"),
                "{name}: {reason}"
            );
        }
        let missing = good.with_file_name("missing");
        assert!(
            read_secret(&missing)
                .expect_err("missing")
                .contains("could not be opened")
        );
        let directory = good.parent().expect("dir").to_path_buf();
        assert!(read_secret(&directory).is_err());
        assert!(good.exists(), "the chosen file is left as it was");
    }
}
