//! A server-sent-events parser for the host's relay of run streams.
//!
//! The relay forwards each event's `id`, `event` and `data` unchanged, so the
//! UI's existing sequencing (ordered ids, gap detection, `?after=` replay in
//! `frontend/src/hooks/useRunEvents.ts`) behaves exactly as with a browser
//! `EventSource`.

use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SseEvent {
    pub id: Option<String>,
    pub event: String,
    pub data: String,
}

/// Incremental parser: feed it byte chunks as they arrive, in any split.
#[derive(Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    id: Option<String>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(end) = self.buffer.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            // A whole line is complete UTF-8 even when a chunk split a character.
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() || self.event.is_some() {
                    events.push(SseEvent {
                        id: self.id.clone(),
                        event: self.event.take().unwrap_or_else(|| "message".to_owned()),
                        data: self.data.join("\n"),
                    });
                    self.data.clear();
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line, ""),
            };
            match field {
                "data" => self.data.push(value.to_owned()),
                "event" => self.event = Some(value.to_owned()),
                // The last id persists across events, as `lastEventId` does.
                "id" if !value.contains('\0') => self.id = Some(value.to_owned()),
                _ => {}
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_survive_any_chunking() {
        let stream = "id: 0\nevent: run.started\ndata: {\"a\":1}\n\n: keep-alive\n\nid: 1\nevent: text\ndata: é\ndata: two\n\n";
        let whole = SseParser::default().push(stream.as_bytes());
        let mut parser = SseParser::default();
        let mut split = Vec::new();
        for byte in stream.as_bytes() {
            split.extend(parser.push(std::slice::from_ref(byte)));
        }
        assert_eq!(whole, split);
        assert_eq!(whole.len(), 2);
        assert_eq!(
            whole[0],
            SseEvent {
                id: Some("0".into()),
                event: "run.started".into(),
                data: "{\"a\":1}".into()
            }
        );
        assert_eq!(whole[1].data, "é\ntwo");
        assert_eq!(whole[1].id.as_deref(), Some("1"));
    }

    #[test]
    fn crlf_and_unnamed_events() {
        let events = SseParser::default().push(b"data: x\r\n\r\n");
        assert_eq!(
            events,
            vec![SseEvent {
                id: None,
                event: "message".into(),
                data: "x".into()
            }]
        );
    }
}
