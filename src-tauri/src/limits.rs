//! Caps on what Relay reads from outside: model servers, MCP servers, web
//! pages and scripts. Without them a server that sends one endless line, a
//! huge body or nothing at all could make Relay hold gigabytes or wait
//! forever.

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt};

/// A JSON body, a stream line or an MCP message: generous (an MCP tool may
/// answer with a screenshot), but bounded.
pub const MAX_JSON: usize = 32 * 1024 * 1024;
/// The part of an error body Relay reads for its message.
pub const MAX_ERROR: usize = 64 * 1024;
/// A reply stream that sends nothing for this long is given up on. Long,
/// because a local server may think over a big prompt before its first
/// word; the user can always press Stop sooner.
pub const IDLE: Duration = Duration::from_secs(15 * 60);

fn too_long(max: usize) -> String {
    format!("The server's answer was longer than {} MB", max / 1024 / 1024)
}

/// The whole body, if it's at most `max` bytes.
pub async fn body(resp: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    if resp.content_length().is_some_and(|n| n > max as u64) {
        return Err(too_long(max));
    }
    let mut out = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("The connection dropped: {e}"))?;
        if out.len() + chunk.len() > max {
            return Err(too_long(max));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// The body as JSON, if it's at most `max` bytes.
pub async fn json(resp: reqwest::Response, max: usize) -> Result<Value, String> {
    let bytes = body(resp, max).await?;
    serde_json::from_slice(&bytes).map_err(|_| "The answer wasn't JSON".to_string())
}

/// The first `max` bytes of a body as text (for error messages); the rest
/// isn't read.
pub async fn text_start(resp: reqwest::Response, max: usize) -> String {
    let mut out = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(Ok(chunk)) = stream.next().await {
        out.extend_from_slice(&chunk);
        if out.len() >= max {
            out.truncate(max);
            break;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Splits a byte stream into lines (server-sent events), none longer than
/// `max`.
#[derive(Default)]
pub struct Lines {
    buf: Vec<u8>,
}

impl Lines {
    /// Adds a chunk and returns the lines it completed, without their `\n`.
    pub fn push(&mut self, chunk: &[u8], max: usize) -> Result<Vec<String>, String> {
        // What was held has no newline in it: look only at the new bytes.
        let mut from = self.buf.len();
        self.buf.extend_from_slice(chunk);
        let mut lines = Vec::new();
        let mut start = 0;
        while let Some(off) = self.buf[from..].iter().position(|&b| b == b'\n') {
            let end = from + off;
            lines.push(String::from_utf8_lossy(&self.buf[start..end]).into_owned());
            start = end + 1;
            from = start;
        }
        self.buf.drain(..start);
        if self.buf.len() > max {
            self.buf = Vec::new();
            return Err(format!("The server sent a line longer than {} MB", max / 1024 / 1024));
        }
        Ok(lines)
    }

    /// What's left when the stream ends (a last line without `\n`).
    pub fn rest(&mut self) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned()
    }
}

/// The next line of `r`, without its `\n`. A line longer than `max` is read
/// and dropped (it comes back empty). None at the end.
pub async fn next_line<R: AsyncBufRead + Unpin>(r: &mut R, max: usize) -> std::io::Result<Option<String>> {
    let mut line: Vec<u8> = Vec::new();
    let mut skipping = false;
    let mut any = false;
    loop {
        let buf = r.fill_buf().await?;
        if buf.is_empty() {
            return Ok(any.then(|| String::from_utf8_lossy(&line).into_owned()));
        }
        any = true;
        let (take, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (i, true),
            None => (buf.len(), false),
        };
        if !skipping {
            if line.len() + take > max {
                skipping = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(&buf[..take]);
            }
        }
        r.consume(if done { take + 1 } else { take });
        if done {
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
    }
}

/// Reads `r` to its end, keeping the first `max` bytes. The rest is read and
/// dropped, so a child process never blocks on a full pipe. True when cut.
pub async fn read_capped<R: AsyncRead + Unpin>(r: &mut R, max: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut cut = false;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match r.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = max.saturating_sub(out.len());
                if n > room {
                    cut = true;
                }
                out.extend_from_slice(&chunk[..n.min(room)]);
            }
        }
    }
    (out, cut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_split_across_chunks_and_stay_bounded() {
        let mut l = Lines::default();
        assert_eq!(l.push(b"data: a\ndata: b", 100).unwrap(), ["data: a"]);
        assert_eq!(l.push("é\n\ndata: c".as_bytes(), 100).unwrap(), ["data: bé", ""]);
        assert_eq!(l.rest(), "data: c");
        // A character split between chunks comes out whole.
        let e = "é".as_bytes();
        assert!(l.push(&e[..1], 100).unwrap().is_empty());
        assert_eq!(l.push(&[e[1], b'\n'], 100).unwrap(), ["é"]);
        // A server that never sends a newline.
        let mut l = Lines::default();
        assert!(l.push(&[b'x'; 60], 100).is_ok());
        assert!(l.push(&[b'x'; 60], 100).is_err());
    }

    #[tokio::test]
    async fn long_lines_are_dropped_not_held() {
        let data = format!("{{\"a\":1}}\n{}\n{{\"b\":2}}", "x".repeat(5000));
        let mut r = tokio::io::BufReader::with_capacity(64, data.as_bytes());
        assert_eq!(next_line(&mut r, 1000).await.unwrap().as_deref(), Some("{\"a\":1}"));
        assert_eq!(next_line(&mut r, 1000).await.unwrap().as_deref(), Some(""));
        assert_eq!(next_line(&mut r, 1000).await.unwrap().as_deref(), Some("{\"b\":2}"));
        assert_eq!(next_line(&mut r, 1000).await.unwrap(), None);
    }

    #[tokio::test]
    async fn script_output_is_cut_but_read_to_the_end() {
        let data = vec![b'y'; 300_000];
        let mut r = &data[..];
        let (kept, cut) = read_capped(&mut r, 1000).await;
        assert_eq!((kept.len(), cut), (1000, true));
        assert!(r.is_empty());
    }

    #[tokio::test]
    async fn bodies_have_a_ceiling() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                // No Content-Length: the body just keeps coming.
                let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n").await;
                for _ in 0..64 {
                    if sock.write_all(&[b' '; 16 * 1024]).await.is_err() {
                        break;
                    }
                }
            }
        });
        let c = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://127.0.0.1:{port}/");
        assert!(body(c.get(&url).send().await.unwrap(), 100_000).await.is_err());
        assert_eq!(text_start(c.get(&url).send().await.unwrap(), 500).await.len(), 500);
        assert_eq!(body(c.get(&url).send().await.unwrap(), 2 * 1024 * 1024).await.unwrap().len(), 64 * 16 * 1024);
    }
}
