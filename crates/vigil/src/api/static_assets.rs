//! Serves the built SPA from `VIGIL_WEB_DIR` (default `/srv/web-dist`).
//! Mounted as the router's fallback service, so any GET not matched by
//! `/api`, `/events`, or `/ping/:token` — including client-side routes like
//! `/incidents` or `/monitors/42` — falls through to `index.html`, the
//! standard single-page-app client-side-routing pattern.

use tower_http::services::{ServeDir, ServeFile};

pub fn service() -> ServeDir<ServeFile> {
    let dir = std::env::var("VIGIL_WEB_DIR").unwrap_or_else(|_| "/srv/web-dist".to_string());
    let index = format!("{dir}/index.html");
    // `.fallback()` (not `.not_found_service()`, which wraps the fallback in
    // `SetStatus` and forces every response through it to report 404) so a
    // deep-linked client route like `/incidents` serves index.html with its
    // natural 200, not a 404.
    ServeDir::new(dir).fallback(ServeFile::new(index))
}
