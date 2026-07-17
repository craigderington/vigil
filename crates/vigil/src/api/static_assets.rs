//! Serves the built SPA from `VIGIL_WEB_DIR` (default `/srv/web-dist`).
//! Mounted as the router's fallback service, so any GET not matched by
//! `/api`, `/events`, or `/ping/:token` — including client-side routes like
//! `/incidents` or `/monitors/42` — falls through to `index.html`, the
//! standard single-page-app client-side-routing pattern.

use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_status::SetStatus;

pub fn service() -> ServeDir<SetStatus<ServeFile>> {
    let dir = std::env::var("VIGIL_WEB_DIR").unwrap_or_else(|_| "/srv/web-dist".to_string());
    let index = format!("{dir}/index.html");
    ServeDir::new(dir).not_found_service(ServeFile::new(index))
}
