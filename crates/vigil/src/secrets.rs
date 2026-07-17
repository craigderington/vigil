//! SMTP password lookup: a mounted secrets file takes precedence (the
//! Docker/Compose `secrets:` convention) over a plain environment variable.

const DEFAULT_SECRET_FILE: &str = "/run/secrets/smtp_password";

/// Resolves the SMTP password. Prefers the file at
/// `VIGIL_SMTP_PASSWORD_FILE` (default `/run/secrets/smtp_password`) when it
/// exists; an existing-but-unreadable file is logged and treated as absent
/// (never silently falls through to the env var in that case — an operator
/// who mounted a secret file expects it to be used, so a permissions bug
/// there should surface as "no password" rather than mask itself). If the
/// file doesn't exist at all, falls back to `VIGIL_SMTP_PASSWORD`.
pub fn read_smtp_password() -> Option<String> {
    let path = std::env::var("VIGIL_SMTP_PASSWORD_FILE")
        .unwrap_or_else(|_| DEFAULT_SECRET_FILE.to_string());

    if std::path::Path::new(&path).exists() {
        return match std::fs::read_to_string(&path) {
            Ok(contents) => Some(contents.trim().to_string()),
            Err(error) => {
                tracing::error!(path = %path, %error, "smtp password file exists but is unreadable");
                None
            }
        };
    }

    std::env::var("VIGIL_SMTP_PASSWORD").ok()
}
