//! Files sent with a message: images, audio, video, PDFs, documents and text.
//!
//! Each one lives in `<data>\attachments\<id>\`: the file itself under its own
//! (cleaned) name, and Relay's files in `x\`: `meta.json` (the `Attachment`),
//! `text.txt` (text pulled out of a PDF or document), `frame-N.jpg` (stills
//! from a video), `sound.wav` (the sound of a video or audio file, 16 kHz
//! mono), `thumb.png` (an image's tile, transparent edges cropped) and
//! `page-N.jpg` (the pages of a PDF without text). The UI makes those, since
//! the webview decodes media and draws PDFs (pdf.js); Relay copies, sorts and
//! pulls text out.
//!
//! The UI shows them through the `relay` protocol (`serve`). When a model is
//! asked, `content_parts` turns them into the API's message parts, following
//! what the model takes (`Caps`).

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::{new_id, Caps};

/// The largest file Relay takes.
pub const MAX_BYTES: u64 = 100 * 1024 * 1024;
/// Videos up to this size go whole to models that watch video; bigger ones go as stills.
const VIDEO_WHOLE_MAX: u64 = 40 * 1024 * 1024;
/// A text file's (or a document's) text is cut here.
const TEXT_MAX_CHARS: usize = 1_000_000;
/// Stills a video may have.
const MAX_FRAMES: usize = 16;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: String,
    /// The file's name as the user had it.
    pub name: String,
    /// "image" | "audio" | "video" | "pdf" | "document" | "text"
    pub kind: String,
    pub mime: String,
    pub size: u64,
    /// The copy's name in its folder.
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Seconds (audio and video).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    /// Video: when each still (`x/frame-N.jpg`) was taken, in seconds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<f64>,
    /// `x/sound.wav` exists.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sound: bool,
    /// PDFs and documents: how much text `x/text.txt` holds (0: none could be read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chars: Option<u64>,
    /// Recorded in Relay (a voice message).
    #[serde(default, skip_serializing_if = "is_false")]
    pub voice: bool,
    /// `x/thumb.png` exists: the square the tiles show.
    #[serde(default, skip_serializing_if = "is_false")]
    pub thumb: bool,
    /// PDFs: how many pages it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_count: Option<u32>,
    /// PDFs without text: its pages as images (`x/page-N.jpg`), sent instead of the text.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pages: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

pub fn root(data_dir: &Path) -> PathBuf {
    data_dir.join("attachments")
}

/// Attachment ids are `db::new_id`s: 12 lowercase letters and digits.
pub fn valid_id(id: &str) -> bool {
    id.len() == 12 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

fn ext_of(name: &str) -> String {
    Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// What a file is, from its name (and its first bytes, for text).
fn kind_of(name: &str, head: &[u8]) -> Option<(&'static str, &'static str)> {
    Some(match ext_of(name).as_str() {
        "png" => ("image", "image/png"),
        "jpg" | "jpeg" | "jfif" => ("image", "image/jpeg"),
        "gif" => ("image", "image/gif"),
        "webp" => ("image", "image/webp"),
        "bmp" => ("image", "image/bmp"),
        "wav" => ("audio", "audio/wav"),
        "mp3" => ("audio", "audio/mpeg"),
        "m4a" => ("audio", "audio/mp4"),
        "aac" => ("audio", "audio/aac"),
        "ogg" | "oga" => ("audio", "audio/ogg"),
        "opus" => ("audio", "audio/opus"),
        "flac" => ("audio", "audio/flac"),
        "weba" => ("audio", "audio/webm"),
        "mp4" | "m4v" => ("video", "video/mp4"),
        "mov" => ("video", "video/quicktime"),
        "webm" => ("video", "video/webm"),
        "mkv" => ("video", "video/x-matroska"),
        "avi" => ("video", "video/x-msvideo"),
        "pdf" => ("pdf", "application/pdf"),
        "docx" => ("document", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "pptx" => ("document", "application/vnd.openxmlformats-officedocument.presentationml.presentation"),
        _ => return looks_like_text(head).then_some(("text", "text/plain")),
    })
}

/// UTF-8 without NUL bytes (a character cut off at the end of `head` is fine).
fn looks_like_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    }
}

/// A name that's safe as a file name in the attachment's folder.
fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String =
        base.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();
    let mut out: String = cleaned.chars().take(120).collect();
    if out.is_empty() || out.eq_ignore_ascii_case("x") {
        out = format!("file.{}", ext_of(name)).trim_end_matches('.').to_string();
    }
    out
}

fn too_big(name: &str) -> String {
    format!("{name} is too big to attach (the limit is {} MB)", MAX_BYTES / 1024 / 1024)
}

fn unsupported(name: &str) -> String {
    match ext_of(name).as_str() {
        "" => format!("Relay can't read {name}"),
        ext => format!("Relay can't read .{ext} files"),
    }
}

fn read_head(path: &Path, n: usize) -> Result<Vec<u8>, String> {
    let mut f = std::fs::File::open(path).map_err(|e| format!("Couldn't open the file: {e}"))?;
    let mut buf = vec![0u8; n];
    let mut got = 0;
    while got < n {
        match f.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(k) => got += k,
            Err(e) => return Err(format!("Couldn't read the file: {e}")),
        }
    }
    buf.truncate(got);
    Ok(buf)
}

/// Copies a file from disk (picked or dropped) into a new attachment.
pub fn add_path(root: &Path, path: &Path) -> Result<Attachment, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("Couldn't open the file: {e}"))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
    if meta.is_dir() {
        return Err(format!("{name} is a folder; attach the files in it instead"));
    }
    if meta.len() > MAX_BYTES {
        return Err(too_big(&name));
    }
    let head = read_head(path, 64 * 1024)?;
    let (kind, mime) = kind_of(&name, &head).ok_or_else(|| unsupported(&name))?;
    let a = new_attachment(root, &name, kind, mime, meta.len())?;
    std::fs::copy(path, root.join(&a.id).join(&a.file)).map_err(|e| format!("Couldn't copy {name}: {e}"))?;
    finish(root, a)
}

/// A new attachment from bytes (pasted, or recorded in Relay).
pub fn add_bytes(root: &Path, name: &str, bytes: &[u8], voice: bool, duration: Option<f64>) -> Result<Attachment, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(too_big(name));
    }
    let head = &bytes[..bytes.len().min(64 * 1024)];
    let (kind, mime) = kind_of(name, head).ok_or_else(|| unsupported(name))?;
    let mut a = new_attachment(root, name, kind, mime, bytes.len() as u64)?;
    a.voice = voice;
    a.duration = duration.filter(|d| d.is_finite() && *d > 0.0);
    std::fs::write(root.join(&a.id).join(&a.file), bytes).map_err(|e| format!("Couldn't save {name}: {e}"))?;
    finish(root, a)
}

fn new_attachment(root: &Path, name: &str, kind: &str, mime: &str, size: u64) -> Result<Attachment, String> {
    let id = new_id();
    std::fs::create_dir_all(root.join(&id).join("x")).map_err(|e| format!("Couldn't make a place for the file: {e}"))?;
    Ok(Attachment {
        id,
        name: name.rsplit(['/', '\\']).next().unwrap_or(name).to_string(),
        kind: kind.into(),
        mime: mime.into(),
        size,
        file: clean_name(name),
        ..Default::default()
    })
}

/// Size for images, text for PDFs and documents; then saves the meta.
fn finish(root: &Path, mut a: Attachment) -> Result<Attachment, String> {
    let dir = root.join(&a.id);
    let path = dir.join(&a.file);
    match a.kind.as_str() {
        "image" => {
            // A JPEG's size can sit after a big EXIF block.
            if let Ok(head) = read_head(&path, 1024 * 1024) {
                if let Some((w, h)) = image_size(&head) {
                    a.width = Some(w);
                    a.height = Some(h);
                }
            }
        }
        "pdf" | "document" => {
            let text = if a.kind == "pdf" { pdf_text(&path, &dir.join("x").join("text.txt")) } else { office_text(&path) };
            let text = tidy(&text.unwrap_or_default());
            a.chars = Some(text.chars().count() as u64);
            std::fs::write(dir.join("x").join("text.txt"), &text).map_err(|e| e.to_string())?;
        }
        _ => {}
    }
    save(root, &a)?;
    Ok(a)
}

fn save(root: &Path, a: &Attachment) -> Result<(), String> {
    let text = serde_json::to_string_pretty(a).map_err(|e| e.to_string())?;
    std::fs::write(root.join(&a.id).join("x").join("meta.json"), text).map_err(|e| e.to_string())
}

pub fn load(root: &Path, id: &str) -> Result<Attachment, String> {
    if !valid_id(id) {
        return Err("That attachment doesn't exist".into());
    }
    let text = std::fs::read_to_string(root.join(id).join("x").join("meta.json")).map_err(|_| "That attachment is gone".to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// A file the UI made for an attachment: a video still (`frame-N.jpg`, taken
/// at `at` seconds), its sound (`sound.wav`), an image's tile (`thumb.png`),
/// a page of a PDF without text (`page-N.jpg`), or a PDF's text when Relay's
/// own reader found none (`text.txt`).
pub fn put(root: &Path, id: &str, file: &str, bytes: &[u8], at: Option<f64>) -> Result<Attachment, String> {
    let mut a = load(root, id)?;
    let x = root.join(id).join("x");
    let numbered = |prefix: &str| file.strip_prefix(prefix).and_then(|f| f.strip_suffix(".jpg")).and_then(|n| n.parse::<usize>().ok());
    if file == "sound.wav" {
        std::fs::write(x.join(file), bytes).map_err(|e| e.to_string())?;
        a.sound = true;
    } else if file == "thumb.png" {
        std::fs::write(x.join(file), bytes).map_err(|e| e.to_string())?;
        a.thumb = true;
    } else if file == "text.txt" {
        if a.kind != "pdf" || a.chars.is_some_and(|n| n > 0) {
            return Err("That file already has its text".into());
        }
        let text = tidy(&String::from_utf8_lossy(bytes));
        a.chars = Some(text.chars().count() as u64);
        std::fs::write(x.join(file), text).map_err(|e| e.to_string())?;
    } else if let Some(n) = numbered("page-") {
        if a.kind != "pdf" || n != a.pages as usize {
            return Err("Pages arrive in order".into());
        }
        std::fs::write(x.join(file), bytes).map_err(|e| e.to_string())?;
        a.pages += 1;
    } else if let Some(n) = numbered("frame-") {
        if n >= MAX_FRAMES || n != a.frames.len() {
            return Err("Stills arrive in order, at most 16".into());
        }
        std::fs::write(x.join(file), bytes).map_err(|e| e.to_string())?;
        a.frames.push(at.unwrap_or(0.0).max(0.0));
    } else {
        return Err(format!("Relay doesn't keep {file}"));
    }
    save(root, &a)?;
    Ok(a)
}

/// What the UI measured: a video's or audio's length, a video's size, a PDF's pages.
pub fn set_info(
    root: &Path,
    id: &str,
    duration: Option<f64>,
    width: Option<u32>,
    height: Option<u32>,
    page_count: Option<u32>,
) -> Result<Attachment, String> {
    let mut a = load(root, id)?;
    if let Some(n) = page_count.filter(|n| *n > 0) {
        a.page_count = Some(n);
    }
    if let Some(d) = duration.filter(|d| d.is_finite() && *d > 0.0) {
        a.duration = Some(d);
    }
    if let (Some(w), Some(h)) = (width.filter(|w| *w > 0), height.filter(|h| *h > 0)) {
        a.width = Some(w);
        a.height = Some(h);
    }
    save(root, &a)?;
    Ok(a)
}

pub fn path_of(root: &Path, a: &Attachment) -> PathBuf {
    root.join(&a.id).join(&a.file)
}

/// Removes these attachments, unless a message still holds them.
pub fn forget(root: &Path, ids: impl IntoIterator<Item = String>, keep: &HashSet<String>) {
    for id in ids {
        if valid_id(&id) && !keep.contains(&id) {
            let _ = std::fs::remove_dir_all(root.join(&id));
        }
    }
}

/// Removes folders no message holds that are older than `min_age` (younger
/// ones may still be waiting in the composer).
pub fn collect(root: &Path, keep: &HashSet<String>, min_age: Duration) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !valid_id(&name) || keep.contains(&name) {
            continue;
        }
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age > min_age);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

// ---- reading files ------------------------------------------------------------------

/// Width and height from a PNG, GIF, JPEG, WebP or BMP header.
fn image_size(b: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32);
    let le16 = |i: usize| Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32);
    let be32 = |i: usize| Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?));
    let le32 = |i: usize| Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?));
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((be32(16)?, be32(20)?));
    }
    if b.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if b.starts_with(b"BM") {
        let h = le32(22)? as i32;
        return Some((le32(18)?, h.unsigned_abs()));
    }
    if b.len() > 30 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return match &b[12..16] {
            b"VP8 " => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            b"VP8L" => {
                let v = le32(21)?;
                Some(((v & 0x3fff) + 1, ((v >> 14) & 0x3fff) + 1))
            }
            b"VP8X" => {
                let w = u32::from_le_bytes([b[24], b[25], b[26], 0]) + 1;
                let h = u32::from_le_bytes([b[27], b[28], b[29], 0]) + 1;
                Some((w, h))
            }
            _ => None,
        };
    }
    if b.starts_with(&[0xff, 0xd8]) {
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xff {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            // Start-of-frame markers (not DHT, JPG or DAC).
            if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
                return Some((be16(i + 7)?, be16(i + 5)?));
            }
            if marker == 0xd8 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) || marker == 0xff {
                i += if marker == 0xff { 1 } else { 2 };
                continue;
            }
            i += 2 + be16(i + 2)? as usize;
        }
    }
    None
}

/// Blank-line runs shortened, trailing spaces gone.
fn tidy(text: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

/// The text of a Word document or a PowerPoint deck.
fn office_text(path: &Path) -> Option<String> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let mut read = |name: &str| -> Option<String> {
        let mut s = String::new();
        zip.by_name(name).ok()?.read_to_string(&mut s).ok()?;
        Some(s)
    };
    if ext_of(&path.to_string_lossy()) == "docx" {
        return Some(xml_text(&read("word/document.xml")?, "w:t", "w:p"));
    }
    let mut slides: Vec<(usize, String)> = zip
        .file_names()
        .filter_map(|n| {
            let num = n.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?.parse().ok()?;
            Some((num, n.to_string()))
        })
        .collect();
    slides.sort();
    let mut out = String::new();
    for (num, name) in slides {
        let mut s = String::new();
        if zip.by_name(&name).ok()?.read_to_string(&mut s).is_ok() {
            out.push_str(&format!("Slide {num}\n{}\n\n", xml_text(&s, "a:t", "a:p")));
        }
    }
    Some(out)
}

/// The text runs (`text_tag`) of Office XML, a line per paragraph (`para_tag`).
fn xml_text(xml: &str, text_tag: &str, para_tag: &str) -> String {
    let mut out = String::new();
    let close = format!("</{text_tag}>");
    let mut i = 0;
    while let Some(lt) = xml[i..].find('<') {
        let start = i + lt;
        let Some(gt) = xml[start..].find('>') else { break };
        let end = start + gt;
        let tag = &xml[start + 1..end];
        let closing = tag.starts_with('/');
        let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        i = end + 1;
        if !closing && name == text_tag && !tag.ends_with('/') {
            if let Some(c) = xml[i..].find(&close) {
                out.push_str(&unescape(&xml[i..i + c]));
                i += c + close.len();
            }
        } else if closing && name == para_tag {
            out.push('\n');
        } else if name == "w:tab" {
            out.push('\t');
        } else if name == "w:br" || name == "a:br" {
            out.push('\n');
        }
    }
    out
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest.find(';').filter(|&k| k < 12) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let ch = match entity {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A PDF's text, read by a copy of Relay started for just that
/// (`relay.exe --pdf-text <pdf> <out>`): a PDF the reader chokes on can't take
/// the app down with it. A minute at most.
fn pdf_text(pdf: &Path, out: &Path) -> Option<String> {
    // Under `cargo test` the current exe is the test runner, not Relay.
    if cfg!(test) {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--pdf-text").arg(pdf).arg(out);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < Duration::from_secs(60) => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    }
    std::fs::read_to_string(out).ok()
}

/// The `--pdf-text` side: reads the PDF and writes its text.
pub fn pdf_text_main(pdf: &str, out: &str) {
    if let Ok(text) = pdf_extract::extract_text(pdf) {
        let _ = std::fs::write(out, text);
    }
}

// ---- the relay:// protocol --------------------------------------------------------

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn decode_header(s: &str) -> String {
    percent_decode(s)
}

fn mime_of(file: &str) -> &'static str {
    match ext_of(file).as_str() {
        "jpg" => "image/jpeg",
        "wav" => "audio/wav",
        "txt" | "json" => "text/plain; charset=utf-8",
        _ => kind_of(file, b"\0").map(|(_, m)| m).unwrap_or("application/octet-stream"),
    }
}

/// Serves `/<id>/<file>` or `/<id>/x/<file>` from the attachments folder,
/// with byte ranges (video seeks) and CORS (the UI reads frames off a canvas).
pub fn serve(root: &Path, path: &str, range: Option<&str>) -> tauri::http::Response<Vec<u8>> {
    use tauri::http::{header, Response, StatusCode};
    let plain = |status: StatusCode| Response::builder().status(status).header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*").body(Vec::new()).unwrap();
    let rel = percent_decode(path.trim_start_matches('/'));
    let segs: Vec<&str> = rel.split('/').collect();
    let safe = |f: &str| !f.is_empty() && f != "." && f != ".." && !f.contains(['\\', ':']) && !f.starts_with('.');
    let full = match segs.as_slice() {
        [id, f] if valid_id(id) && safe(f) => root.join(id).join(f),
        [id, "x", f] if valid_id(id) && safe(f) => root.join(id).join("x").join(f),
        _ => return plain(StatusCode::NOT_FOUND),
    };
    let Ok(mut file) = std::fs::File::open(&full) else { return plain(StatusCode::NOT_FOUND) };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mime = mime_of(segs.last().unwrap_or(&""));
    let base = || {
        Response::builder()
            .header(header::CONTENT_TYPE, mime)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header(header::CACHE_CONTROL, "max-age=31536000, immutable")
    };
    // "bytes=a-b", "bytes=a-" (open: a few MB at a time) or "bytes=-n" (the last n).
    let wanted = range.and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.split(',').next()).and_then(|r| {
        let (a, b) = r.trim().split_once('-')?;
        if a.is_empty() {
            let n: u64 = b.parse().ok()?;
            return Some((len.saturating_sub(n), len.saturating_sub(1)));
        }
        let start: u64 = a.parse().ok()?;
        let end = if b.is_empty() { start + 8 * 1024 * 1024 - 1 } else { b.parse().ok()? };
        Some((start, end.min(len.saturating_sub(1))))
    });
    match wanted {
        Some((start, end)) if start < len && start <= end => {
            let mut buf = vec![0u8; (end - start + 1) as usize];
            if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut buf).is_err() {
                return plain(StatusCode::INTERNAL_SERVER_ERROR);
            }
            base()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))
                .body(buf)
                .unwrap()
        }
        Some(_) => Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{len}"))
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .body(Vec::new())
            .unwrap(),
        None => {
            let mut buf = Vec::with_capacity(len as usize);
            if file.read_to_end(&mut buf).is_err() {
                return plain(StatusCode::INTERNAL_SERVER_ERROR);
            }
            base().status(StatusCode::OK).body(buf).unwrap()
        }
    }
}

// ---- for the model ---------------------------------------------------------------------

fn data_url(path: &Path, mime: &str) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
}

fn text(t: impl Into<String>) -> Value {
    json!({ "type": "text", "text": t.into() })
}

/// A file's text as the model sees it.
fn file_block(name: &str, body: &str) -> String {
    let mut body: String = body.trim_end().to_string();
    let total = body.chars().count();
    if total > TEXT_MAX_CHARS {
        body = body.chars().take(TEXT_MAX_CHARS).collect();
        body.push_str(&format!("\n[… {} more characters not included]", total - TEXT_MAX_CHARS));
    }
    format!("<file name=\"{}\">\n{body}\n</file>", name.replace('"', "'"))
}

/// "1:05" from seconds.
fn clock(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// The audio to send and its format: the original when it's WAV or MP3 (what
/// every server takes), else the WAV Relay made of it.
fn audio_source(root: &Path, a: &Attachment) -> (PathBuf, String) {
    let ext = ext_of(&a.file);
    if ext == "wav" || ext == "mp3" || !a.sound {
        return (path_of(root, a), if ext.is_empty() { "wav".into() } else { ext });
    }
    (root.join(&a.id).join("x").join("sound.wav"), "wav".into())
}

fn audio_part(path: &Path, format: &str) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    Some(json!({
        "type": "input_audio",
        "input_audio": { "data": base64::engine::general_purpose::STANDARD.encode(bytes), "format": format }
    }))
}

/// The message parts for a user message's files, following what the model
/// takes. Images and audio always go as themselves (the server answers if the
/// model can't take them); video goes whole to video models, else as stills
/// (and its sound, to models that hear); PDFs go as files where the server
/// reads them, else as their text.
pub fn content_parts(root: &Path, list: &[Attachment], caps: Caps) -> Vec<Value> {
    let mut parts = Vec::new();
    for a in list {
        let path = path_of(root, a);
        let gone = || text(format!("[{} was attached, but the file is no longer there]", a.name));
        match a.kind.as_str() {
            "image" => match data_url(&path, &a.mime) {
                Some(url) => parts.push(json!({ "type": "image_url", "image_url": { "url": url } })),
                None => parts.push(gone()),
            },
            "audio" => {
                let (src, format) = audio_source(root, a);
                match audio_part(&src, &format) {
                    Some(p) => {
                        if !a.voice {
                            parts.push(text(format!("[Audio: {}]", a.name)));
                        }
                        parts.push(p);
                    }
                    None => parts.push(gone()),
                }
            }
            "video" => {
                if caps.video && a.size <= VIDEO_WHOLE_MAX {
                    match data_url(&path, &a.mime) {
                        Some(url) => {
                            parts.push(text(format!("[Video: {}]", a.name)));
                            parts.push(json!({ "type": "video_url", "video_url": { "url": url } }));
                        }
                        None => parts.push(gone()),
                    }
                    continue;
                }
                let length = a.duration.map(|d| format!(", {}", clock(d))).unwrap_or_default();
                if a.frames.is_empty() {
                    parts.push(text(format!("[Video: {}{length}. Relay couldn't take stills from it.]", a.name)));
                } else {
                    let at: Vec<String> = a.frames.iter().map(|t| clock(*t)).collect();
                    parts.push(text(format!("[Video: {}{length}, as {} stills taken at {}]", a.name, a.frames.len(), at.join(", "))));
                    for i in 0..a.frames.len() {
                        if let Some(url) = data_url(&root.join(&a.id).join("x").join(format!("frame-{i}.jpg")), "image/jpeg") {
                            parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
                        }
                    }
                }
                if caps.audio && a.sound {
                    if let Some(p) = audio_part(&root.join(&a.id).join("x").join("sound.wav"), "wav") {
                        parts.push(text(format!("[The sound of {}]", a.name)));
                        parts.push(p);
                    }
                }
            }
            "pdf" if caps.pdf => match data_url(&path, "application/pdf") {
                Some(url) => parts.push(json!({ "type": "file", "file": { "filename": a.name, "file_data": url } })),
                None => parts.push(gone()),
            },
            // No text: its pages, as images.
            "pdf" if a.pages > 0 => {
                let n = a.pages;
                parts.push(text(format!("[PDF: {}, {n} page{} as images]", a.name, if n == 1 { "" } else { "s" })));
                for i in 0..n {
                    if let Some(url) = data_url(&root.join(&a.id).join("x").join(format!("page-{i}.jpg")), "image/jpeg") {
                        parts.push(json!({ "type": "image_url", "image_url": { "url": url } }));
                    }
                }
            }
            "pdf" | "document" => {
                let body = std::fs::read_to_string(root.join(&a.id).join("x").join("text.txt")).unwrap_or_default();
                let body = if body.trim().is_empty() {
                    "(No text could be read from this file. It may be scanned pages or pictures.)".to_string()
                } else {
                    body
                };
                parts.push(text(file_block(&a.name, &body)));
            }
            _ => match std::fs::read(&path) {
                Ok(bytes) => parts.push(text(file_block(&a.name, &String::from_utf8_lossy(&bytes)))),
                Err(_) => parts.push(gone()),
            },
        }
    }
    parts
}

/// What a message's files were, in words: for titles and the Markdown export.
pub fn describe(list: &[Attachment]) -> String {
    list.iter()
        .map(|a| if a.voice { format!("Voice message ({})", clock(a.duration.unwrap_or(0.0))) } else { a.name.clone() })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_files() {
        assert_eq!(kind_of("a.PNG", b"").unwrap().0, "image");
        assert_eq!(kind_of("talk.m4a", b"").unwrap().0, "audio");
        assert_eq!(kind_of("clip.webm", b"").unwrap().0, "video");
        assert_eq!(kind_of("notes.rs", b"fn main() {}").unwrap(), ("text", "text/plain"));
        assert!(kind_of("blob.bin", b"\x00\x01\x02").is_none());
        // A UTF-8 character cut at the end of the sample is still text.
        assert!(looks_like_text("caf\u{e9}".as_bytes().split_last().unwrap().1));
    }

    #[test]
    fn cleans_names() {
        assert_eq!(clean_name("..\\evil/../x:y?.txt"), "x_y_.txt");
        assert_eq!(clean_name("x"), "file");
        assert_eq!(clean_name("   "), "file");
    }

    #[test]
    fn reads_image_sizes() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(image_size(&png), Some((640, 480)));
        let gif = b"GIF89a\x20\x03\x58\x02";
        assert_eq!(image_size(gif), Some((800, 600)));
        // JPEG: SOI, an APP0 segment, then SOF0 with height 300 and width 400.
        let jpg = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x01, 0x2c, 0x01, 0x90, 0x03, 0x00, 0x00];
        assert_eq!(image_size(&jpg), Some((400, 300)));
    }

    #[test]
    fn reads_office_xml() {
        let xml = r#"<w:body><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space="preserve"> &amp; bye</w:t></w:r></w:p><w:p><w:r><w:tab/><w:t>Two</w:t></w:r></w:p></w:body>"#;
        assert_eq!(xml_text(xml, "w:t", "w:p"), "Hello & bye\n\tTwo\n");
        assert_eq!(unescape("a &lt;b&gt; &#233; &#x41; & c"), "a <b> é A & c");
    }

    #[test]
    fn decodes_paths() {
        assert_eq!(percent_decode("my%20file%C3%A9.txt"), "my fileé.txt");
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn serves_only_attachment_files() {
        let dir = std::env::temp_dir().join(format!("relay-att-{}", new_id()));
        let root = root(&dir);
        let a = add_bytes(&root, "hello.txt", b"hello world", false, None).unwrap();
        let ok = serve(&root, &format!("/{}/{}", a.id, a.file), None);
        assert_eq!(ok.status(), 200);
        assert_eq!(ok.body(), b"hello world");
        let part = serve(&root, &format!("/{}/{}", a.id, a.file), Some("bytes=6-"));
        assert_eq!(part.status(), 206);
        assert_eq!(part.body(), b"world");
        assert_eq!(serve(&root, &format!("/{}/../x", a.id), None).status(), 404);
        assert_eq!(serve(&root, "/../../secret.txt", None).status(), 404);
        assert_eq!(serve(&root, &format!("/{}/x/meta.json", a.id), None).status(), 200);
        let parts = content_parts(&root, &[a.clone()], Caps::default());
        assert_eq!(parts[0]["text"], "<file name=\"hello.txt\">\nhello world\n</file>");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn video_goes_as_stills_unless_the_model_watches() {
        let dir = std::env::temp_dir().join(format!("relay-att-{}", new_id()));
        let root = root(&dir);
        let a = add_bytes(&root, "clip.mp4", b"not really a video", false, None).unwrap();
        put(&root, &a.id, "frame-0.jpg", b"jpg0", Some(0.0)).unwrap();
        let a = put(&root, &a.id, "frame-1.jpg", b"jpg1", Some(5.0)).unwrap();
        assert!(put(&root, &a.id, "frame-5.jpg", b"x", None).is_err());
        let a = set_info(&root, &a.id, Some(10.0), Some(640), Some(360), None).unwrap();
        let stills = content_parts(&root, &[a.clone()], Caps { vision: true, ..Default::default() });
        assert_eq!(stills.len(), 3);
        assert_eq!(stills[0]["text"], "[Video: clip.mp4, 0:10, as 2 stills taken at 0:00, 0:05]");
        assert_eq!(stills[1]["type"], "image_url");
        let whole = content_parts(&root, &[a], Caps { vision: true, video: true, ..Default::default() });
        assert_eq!(whole[1]["type"], "video_url");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pdfs_without_text_go_as_pages() {
        let dir = std::env::temp_dir().join(format!("relay-att-{}", new_id()));
        let root = root(&dir);
        // Not a real PDF: Relay's reader finds no text in it.
        let a = add_bytes(&root, "scan.pdf", b"%PDF-1.4 nothing", false, None).unwrap();
        assert_eq!(a.chars, Some(0));
        assert!(put(&root, &a.id, "page-1.jpg", b"x", None).is_err());
        put(&root, &a.id, "page-0.jpg", b"jpg0", None).unwrap();
        let a = put(&root, &a.id, "page-1.jpg", b"jpg1", None).unwrap();
        let parts = content_parts(&root, &[a.clone()], Caps { vision: true, ..Default::default() });
        assert_eq!(parts[0]["text"], "[PDF: scan.pdf, 2 pages as images]");
        assert_eq!(parts.len(), 3);
        // A server that reads PDFs still gets the file.
        assert_eq!(content_parts(&root, &[a.clone()], Caps { pdf: true, ..Default::default() })[0]["type"], "file");
        // Text found by the UI's reader fills in once, and only when Relay's found none.
        let a = put(&root, &a.id, "text.txt", b"Scanned words", None).unwrap();
        assert_eq!(a.chars, Some(13));
        assert!(put(&root, &a.id, "text.txt", b"again", None).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
