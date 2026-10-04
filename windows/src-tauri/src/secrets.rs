// API keys live in the Windows Credential Manager or, on Linux, the Secret
// Service (GNOME Keyring, KWallet) — never on disk and never in the front end — the island can only ask whether a key is present.

use keyring::Entry;

const SERVICE: &str = "fr.louisraille.coucou";

/// Every key Coucou may store. Anything outside this list is refused.
pub const KNOWN_KEYS: &[&str] = &[
    "anthropic-api-key",
    "n8n-url",
    "n8n-api-key",
    "vercel-token",
    "github-token",
    "stripe-api-key",
    "resend-api-key",
    "notion-api-key",
    "calcom-api-key",
];

/// The key holding a "Send to…" destination's webhook `url` or its `secret`.
/// Destinations are added by the user, so their keys cannot be listed above.
pub fn webhook_key(id: &str, field: &str) -> String {
    format!("webhook-{id}-{field}")
}

/// `webhook-<id>-url` or `webhook-<id>-secret`, with an id of 1 to 16 lowercase
/// letters and digits — the only keys accepted besides the known ones.
fn is_webhook_key(key: &str) -> bool {
    let Some(rest) = key.strip_prefix("webhook-") else { return false };
    let Some(id) = rest.strip_suffix("-url").or_else(|| rest.strip_suffix("-secret")) else { return false };
    (1..=16).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

fn entry(key: &str) -> Option<Entry> {
    if !KNOWN_KEYS.contains(&key) && !is_webhook_key(key) {
        return None;
    }
    Entry::new(SERVICE, key).ok()
}

pub fn get(key: &str) -> Option<String> {
    entry(key)?.get_password().ok().filter(|v| !v.is_empty())
}

pub fn set(key: &str, value: &str) -> Result<(), String> {
    let entry = entry(key).ok_or_else(|| format!("unknown key {key}"))?;
    if value.is_empty() {
        let _ = entry.delete_credential();
        return Ok(());
    }
    entry.set_password(value).map_err(|e| e.to_string())
}

pub fn clear(key: &str) -> Result<(), String> {
    let entry = entry(key).ok_or_else(|| format!("unknown key {key}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn present(key: &str) -> bool {
    get(key).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_well_formed_webhook_keys_are_accepted() {
        assert!(is_webhook_key(&webhook_key("a1b2c3d4", "url")));
        assert!(is_webhook_key(&webhook_key("a1b2c3d4", "secret")));
        for key in ["webhook--url", "webhook-ABC-url", "webhook-a b-url", "webhook-abc-token", "webhook-abc",
                    "webhook-aaaaaaaaaaaaaaaaa-url", "anthropic-api-key", "webhook-../x-url"] {
            assert!(!is_webhook_key(key), "{key} is not a webhook key");
        }
    }
}
