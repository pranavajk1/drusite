// SPDX-License-Identifier: AGPL-3.0-or-later

//! Server-sent events, the format every vendor streams chats in.
//!
//! Only what chats need: `event:` and `data:` fields, comments (OpenRouter sends
//! `: OPENROUTER PROCESSING` while it waits) ignored, any line ending, events split across
//! chunks anywhere, even inside a UTF-8 character.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub(crate) struct Decoder {
    buf: Vec<u8>,
    event: Option<String>,
    data: Option<String>,
    /// The previous chunk ended in `\r`: a `\n` starting this one belongs to it.
    after_cr: bool,
}

impl Decoder {
    /// Takes the next bytes; returns the events they complete.
    pub(crate) fn push(&mut self, mut chunk: &[u8]) -> Vec<SseEvent> {
        if self.after_cr && chunk.first() == Some(&b'\n') {
            chunk = &chunk[1..];
        }
        self.after_cr = false;
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut start = 0;
        let mut i = 0;
        while i < self.buf.len() {
            let b = self.buf[i];
            if b == b'\n' || b == b'\r' {
                let line = String::from_utf8_lossy(&self.buf[start..i]).into_owned();
                if b == b'\r' {
                    if i + 1 < self.buf.len() {
                        if self.buf[i + 1] == b'\n' {
                            i += 1;
                        }
                    } else {
                        self.after_cr = true;
                    }
                }
                i += 1;
                start = i;
                if let Some(event) = self.line(&line) {
                    out.push(event);
                }
            } else {
                i += 1;
            }
        }
        self.buf.drain(..start);
        out
    }

    /// The stream ended: an event without its closing blank line still counts.
    pub(crate) fn finish(&mut self) -> Option<SseEvent> {
        let rest = std::mem::take(&mut self.buf);
        if !rest.is_empty() {
            let line = String::from_utf8_lossy(&rest).into_owned();
            if let Some(event) = self.line(&line) {
                return Some(event);
            }
        }
        self.line("")
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            let data = self.data.take()?;
            return Some(SseEvent {
                event: self.event.take(),
                data,
            });
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            },
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut d = Decoder::default();
        let mut out: Vec<_> = chunks.iter().flat_map(|c| d.push(c)).collect();
        out.extend(d.finish());
        out
    }

    fn ev(event: Option<&str>, data: &str) -> SseEvent {
        SseEvent {
            event: event.map(str::to_owned),
            data: data.to_owned(),
        }
    }

    #[test]
    fn splits_events_however_the_bytes_arrive() {
        let text = "event: a\ndata: {\"x\":1}\n\n: comment\n\ndata: one\ndata: two\n\ndata:é\n\n";
        let want = vec![
            ev(Some("a"), "{\"x\":1}"),
            ev(None, "one\ntwo"),
            ev(None, "é"),
        ];
        assert_eq!(all(&[text.as_bytes()]), want);
        // Every split point, including inside "é".
        let bytes = text.as_bytes();
        for cut in 0..bytes.len() {
            assert_eq!(all(&[&bytes[..cut], &bytes[cut..]]), want, "cut at {cut}");
        }
        // CRLF and lone CR line endings, a CRLF split between chunks.
        assert_eq!(
            all(&[b"data: a\r", b"\n\r\ndata: b\r\r"]),
            vec![ev(None, "a"), ev(None, "b")]
        );
        // No closing blank line.
        assert_eq!(all(&[b"data: [DONE]"]), vec![ev(None, "[DONE]")]);
    }
}
