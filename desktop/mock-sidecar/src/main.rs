//! A stand-in runtime for desktop tests (D2), standard library only.
//!
//! It implements the desktop side of `lightagent serve --desktop` as specified
//! in docs/desktop/DESKTOP_ARCHITECTURE.md §4 and §6: the key arrives on
//! stdin, the server binds 127.0.0.1 on an ephemeral port and prints one
//! handshake line, and every request must carry the exact Host, no Origin,
//! and the bearer key. Responses are fixed; the real runtime replaces this
//! in D3.
//!
//! `LIGHTAGENT_MOCK_REPORT_VERSION` makes it report another version, to test
//! the host's mismatch handling.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let mut key = String::new();
    if std::io::stdin().read_line(&mut key).is_err() {
        std::process::exit(2);
    }
    let key = key.trim().to_owned();
    if key.len() < 32 {
        eprintln!("mock runtime: no credential on stdin");
        std::process::exit(2);
    }
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("mock runtime: cannot bind: {error}");
            std::process::exit(1);
        }
    };
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(_) => std::process::exit(1),
    };
    let version =
        std::env::var("LIGHTAGENT_MOCK_REPORT_VERSION").unwrap_or_else(|_| VERSION.to_owned());
    println!(
        "{{\"lightagent\":\"serve\",\"protocol\":1,\"version\":\"{version}\",\"addr\":\"127.0.0.1:{port}\",\"profile\":\"default\",\"capabilities\":[\"mock\"]}}"
    );
    eprintln!("mock runtime listening on 127.0.0.1:{port}");
    let host = format!("127.0.0.1:{port}");
    let bearer = format!("Bearer {key}");
    for stream in listener.incoming().flatten() {
        let host = host.clone();
        let bearer = bearer.clone();
        std::thread::spawn(move || {
            let _ = serve(stream, &host, &bearer);
        });
    }
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn read_request(stream: &TcpStream) -> std::io::Result<Request> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<u64>().ok())
        .unwrap_or(0)
        .min(32 * 1024 * 1024);
    let mut body = Vec::new();
    reader.take(length).read_to_end(&mut body)?;
    Ok(Request {
        method,
        path,
        headers,
    })
}

fn respond(
    mut stream: &TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn serve(stream: TcpStream, host: &str, bearer: &str) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let request = read_request(&stream)?;
    let json = "application/json";
    // The same three checks, in the same order, as the real desktop mode.
    if request.header("host") != Some(host) {
        return respond(
            &stream,
            "403 Forbidden",
            json,
            "{\"error\":{\"category\":\"host_rejected\"}}",
        );
    }
    if request.header("origin").is_some() {
        return respond(
            &stream,
            "403 Forbidden",
            json,
            "{\"error\":{\"category\":\"origin_rejected\"}}",
        );
    }
    if request.header("authorization") != Some(bearer) {
        return respond(
            &stream,
            "401 Unauthorized",
            json,
            "{\"error\":{\"category\":\"unauthenticated\"}}",
        );
    }
    let path = request.path.split('?').next().unwrap_or("");
    let route = path.strip_prefix("/api/lightagent/v1").unwrap_or(path);
    if request.method == "GET" && route.starts_with("/runs/") && route.ends_with("/events") {
        let mut stream = &stream;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n"
        )?;
        write!(stream, "id: 0\nevent: run.started\ndata: {{}}\n\n")?;
        write!(
            stream,
            "id: 1\nevent: run.completed\ndata: {{\"reason\":\"mock\"}}\n\n"
        )?;
        return Ok(());
    }
    let body = match (request.method.as_str(), route) {
        ("GET", "/health") => "{\"status\":\"ok\",\"service\":\"lightagent-mock\"}",
        ("GET", "/tools") => {
            "{\"tools\":[{\"name\":\"datetime.now\",\"risk\":\"read\",\"description\":\"Mock runtime tool (D2).\"}]}"
        }
        ("GET", "/skills") => "{\"skills\":[]}",
        ("GET", "/workers") => "{\"workers\":[]}",
        ("GET", "/approvals") => "{\"approvals\":[]}",
        ("GET", "/sessions") => "{\"sessions\":[]}",
        ("GET", "/sessions/search") => "{\"sessions\":[]}",
        ("GET", "/profiles") => {
            "{\"active_profile\":\"default\",\"profiles\":[{\"id\":\"default\",\"name\":\"Default\",\"model\":\"mock\",\"active\":true}]}"
        }
        ("GET", "/provider") => {
            "{\"provider\":\"mock\",\"base_url\":\"http://127.0.0.1:9\",\"configured_model\":\"mock\",\"models\":[\"mock\"],\"model_aliases\":{},\"model_catalog\":{},\"runtime_models\":[],\"streaming\":true,\"tool_calls\":true,\"reasoning_content\":false}"
        }
        ("GET", "/settings") => SETTINGS,
        ("GET", "/jev") => JEV,
        _ => {
            return respond(
                &stream,
                "501 Not Implemented",
                json,
                "{\"error\":{\"category\":\"unavailable\",\"message\":\"not implemented by the D2 mock runtime\"}}",
            );
        }
    };
    respond(&stream, "200 OK", json, body)
}

const SETTINGS: &str = concat!(
    "{\"max_turns\":8,\"max_tool_calls\":16,\"wall_clock_secs\":null,\"approval_policy\":\"balanced\",",
    "\"web_enabled\":false,\"filesystem_tools_enabled\":false,\"terminal_enabled\":false,\"memory_enabled\":true,",
    "\"show_reasoning_in_tui\":true,\"subagents_enabled\":false,\"delegate_timeout_secs\":120,",
    "\"jev\":{\"enabled\":false,\"base_url\":null,\"api_key_configured\":false,\"model\":\"jev\",\"confidence_threshold\":0.8,",
    "\"allowed_models\":[],\"allowed_profiles\":[],\"timeout_secs\":10},",
    "\"qdrant\":{\"enabled\":false,\"base_url\":null,\"api_key_configured\":false,\"collection\":\"lightagent\",\"timeout_secs\":10},",
    "\"infinity\":{\"enabled\":false,\"base_url\":null,\"api_key_configured\":false,\"embedding_model\":\"\",\"rerank_model\":\"\",\"timeout_secs\":10},",
    "\"open_terminal\":{\"enabled\":false,\"base_url\":null,\"api_key_configured\":false,\"request_timeout_secs\":10,",
    "\"execution_timeout_secs\":60,\"poll_interval_ms\":250,\"max_output_bytes\":65536}}"
);

const JEV: &str = concat!(
    "{\"name\":\"Jev\",\"provider\":\"mock\",\"kind\":\"classifier\",\"description\":\"Not configured in the mock runtime.\",",
    "\"profile\":\"default\",\"endpoint_configured\":false,\"api_key_configured\":false,\"api_key_resolvable\":false,",
    "\"configured\":false,\"enabled\":false,\"model\":\"jev\",\"verification\":null,",
    "\"classification\":{\"mode\":\"off\",\"profile_permitted\":false,\"max_input_chars\":2000,\"default_threshold\":0.8,",
    "\"version_thresholds\":{},\"max_calls_per_run\":1},",
    "\"explicit_tool\":{\"name\":\"jev.classify\",\"enabled\":false,\"available\":false},",
    "\"permissions\":{\"risk\":\"read\",\"requirements\":[]},\"last_classification\":null}"
);

#[cfg(test)]
mod tests {
    #[test]
    fn fixtures_are_json_shaped() {
        for fixture in [super::SETTINGS, super::JEV] {
            assert!(fixture.starts_with('{') && fixture.ends_with('}'));
            assert_eq!(fixture.matches('{').count(), fixture.matches('}').count());
        }
    }
}
