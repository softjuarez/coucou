// "Send to…" — hands a dropped file to a webhook the user set up, typically an
// n8n workflow that passes it on to WhatsApp, Teams or anything else. Coucou
// only knows a name and a URL: which service it ends up in, and with which
// credentials, is the workflow's business.
//
// The URL and the optional shared secret live in the keychain like every other
// key. Nothing is ever sent without a click on Send.

use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::multipart::{Form, Part};
use serde_json::Value;

use crate::{files, log, secrets};

const TIMEOUT: Duration = Duration::from_secs(60);
/// Chat services refuse large attachments anyway; past this nothing is sent.
const MAX_FILE: u64 = 25 * 1024 * 1024;
/// The header the receiving workflow checks against the shared secret.
const SECRET_HEADER: &str = "X-Coucou-Secret";

pub struct Delivery {
    /// The destination's id, as stored in the settings.
    pub id: String,
    /// Its name, for messages and the log.
    pub name: String,
    pub file: Option<String>,
    /// The name the file had when it was dropped; the inbox copy may have been
    /// renamed to "name (2).pdf" to keep an earlier one.
    pub file_name: Option<String>,
    pub recipient: String,
    pub message: String,
}

/// Posts the file and its note to the destination's webhook as a multipart form:
/// `destination`, `recipient`, `message`, `fileName` and the file itself as `file`.
pub async fn send(delivery: Delivery) -> Result<(), String> {
    let name = &delivery.name;
    let url = secrets::get(&secrets::webhook_key(&delivery.id, "url"))
        .ok_or_else(|| format!("{name} has no webhook URL. Open settings."))?;
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("{name}: the webhook URL must start with http:// or https://."));
    }

    let mut form = Form::new()
        .text("destination", name.clone())
        .text("recipient", delivery.recipient.trim().to_string())
        .text("message", delivery.message.trim().to_string());

    if let Some(path) = delivery.file.as_deref() {
        let path = inbox_file(path)?;
        let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        if size > MAX_FILE {
            return Err("The file is over 25 MB, too large to send.".into());
        }
        let file_name = sent_name(delivery.file_name.as_deref(), &path);
        let bytes = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
        let part = Part::bytes(bytes)
            .file_name(file_name.clone())
            .mime_str(mime_of(&path))
            .map_err(|e| e.to_string())?;
        form = form.text("fileName", file_name).part("file", part);
    }

    let client = reqwest::Client::builder().timeout(TIMEOUT).build().map_err(|e| e.to_string())?;
    let mut request = client.post(&url).multipart(form);
    if let Some(secret) = secrets::get(&secrets::webhook_key(&delivery.id, "secret")) {
        request = request.header(SECRET_HEADER, secret);
    }

    let response = request.send().await.map_err(|e| {
        // reqwest's error carries the URL, which may hold a token: say what
        // happened, not where.
        let why = if e.is_timeout() { "it did not answer in time" } else { "it could not be reached" };
        format!("{name}: {why}.")
    })?;

    let status = response.status();
    // The name and the status only: the URL and what was sent stay out of the log.
    log::line(format!("webhook {name} answered {}", status.as_u16()));
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    Err(format!("{name} answered {}{}", status.as_u16(), failure_detail(&body)))
}

/// Only a file Coucou copied into its own inbox may be sent: the page names the
/// path, and it must not be able to name any file on the disk.
fn inbox_file(path: &str) -> Result<PathBuf, String> {
    let refused = || "Only a file dropped on the island can be sent.".to_string();
    let file = Path::new(path).canonicalize().map_err(|_| refused())?;
    let inbox = files::inbox_dir().canonicalize().map_err(|_| refused())?;
    if file.is_file() && file.starts_with(&inbox) {
        Ok(file)
    } else {
        Err(refused())
    }
}

/// The name the file travels under: the one it was dropped with when it is a
/// plain file name, the inbox copy's otherwise.
fn sent_name(original: Option<&str>, path: &Path) -> String {
    let plain = |name: &&str| !name.is_empty() && !name.contains(['/', '\\', ':', '"', '\r', '\n']);
    match original.map(str::trim).filter(plain) {
        Some(name) => name.to_string(),
        None => path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into()),
    }
}

/// n8n and most services explain a refusal in a JSON `message`; a short one is
/// worth showing, anything else is noise.
fn failure_detail(body: &str) -> String {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string));
    match message {
        Some(m) if !m.is_empty() => format!(": {}", m.chars().take(140).collect::<String>()),
        _ => ".".into(),
    }
}

fn mime_of(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "txt" | "md" | "log" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "zip" => "application/zip",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_file_in_the_inbox_may_be_sent() {
        let inbox = files::inbox_dir();
        std::fs::create_dir_all(&inbox).unwrap();
        let inside = inbox.join(format!("coucou-webhook-test-{}.txt", std::process::id()));
        std::fs::write(&inside, b"hello").unwrap();
        assert!(inbox_file(inside.to_str().unwrap()).is_ok());

        let outside = std::env::temp_dir().join(format!("coucou-webhook-test-{}.txt", std::process::id()));
        std::fs::write(&outside, b"hello").unwrap();
        assert!(inbox_file(outside.to_str().unwrap()).is_err());
        // Climbing out of the inbox by name is still outside it.
        let climbing = inbox.join("..").join("settings.json");
        assert!(inbox_file(climbing.to_str().unwrap()).is_err());
        assert!(inbox_file(inbox.to_str().unwrap()).is_err(), "a folder is not a file");

        let _ = std::fs::remove_file(inside);
        let _ = std::fs::remove_file(outside);
    }

    #[test]
    fn a_refusal_shows_the_services_own_words() {
        assert_eq!(failure_detail(r#"{"code":404,"message":"The requested webhook is not registered."}"#), ": The requested webhook is not registered.");
        assert_eq!(failure_detail("<html>Bad gateway</html>"), ".");
        assert_eq!(failure_detail(""), ".");
    }

    #[test]
    fn the_file_keeps_the_name_it_was_dropped_with() {
        let copy = Path::new("C:/inbox/minuta (3).docx");
        assert_eq!(sent_name(Some("minuta.docx"), copy), "minuta.docx");
        assert_eq!(sent_name(None, copy), "minuta (3).docx");
        assert_eq!(sent_name(Some("  "), copy), "minuta (3).docx");
        // Not a plain name: fall back to the copy's own.
        assert_eq!(sent_name(Some("..\\secreto.txt"), copy), "minuta (3).docx");
        assert_eq!(sent_name(Some("a\"b.txt"), copy), "minuta (3).docx");
    }

    #[test]
    fn files_are_named_by_their_type() {
        assert_eq!(mime_of(Path::new("Informe.PDF")), "application/pdf");
        assert_eq!(mime_of(Path::new("foto.jpeg")), "image/jpeg");
        assert_eq!(mime_of(Path::new("sin-extension")), "application/octet-stream");
    }
}
