//! Admin-token auth + anonymous session accounting.
//!
//! There is exactly one privileged role — the operator — identified by a
//! random token minted at startup. The operator visits
//! `http://host/?token=<token>`; the index handler stores it as an
//! `rtlfm_admin` HttpOnly cookie, and from then on every request carries it.
//! [`require_admin`] gates the admin-only routes (rescan + the full debug
//! API + arbitrary-station audio) by checking the cookie (or the `?token=`
//! query, so the very first link works before the cookie is set).
//!
//! Everyone else is an anonymous public user: no sign-in, but still
//! accounted for. The index handler issues each a random `rtlfm_anon`
//! cookie and [`track_session`] records it on every request, so the server
//! has a live count of distinct public sessions even though they never
//! authenticate. Public users may only vote, read the info SSE/snapshots,
//! and play the voted-winner audio stream.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use parking_lot::Mutex;
use rand::Rng;
use serde::Serialize;

use crate::api::AppState;

pub const ADMIN_COOKIE: &str = "rtlfm_admin";
pub const ANON_COOKIE: &str = "rtlfm_anon";

/// Mint a 128-bit random hex token (admin token or anon session id).
pub fn gen_token() -> String {
    format!("{:032x}", rand::thread_rng().gen::<u128>())
}

/// Constant-time string compare so the token check can't be timed.
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Parse the `Cookie:` request header into a name→value map.
pub fn parse_cookies(headers: &HeaderMap) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(h) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        for part in h.split(';') {
            if let Some((k, v)) = part.trim().split_once('=') {
                map.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    map
}

fn token_from_query(query: Option<&str>) -> Option<String> {
    query?.split('&').find_map(|p| {
        p.strip_prefix("token=")
            .map(|v| v.to_string())
    })
}

/// True if the request presents the admin token via `?token=` or the
/// `rtlfm_admin` cookie.
pub fn is_admin(headers: &HeaderMap, query: Option<&str>, token: &str) -> bool {
    if let Some(t) = token_from_query(query) {
        if ct_eq(&t, token) {
            return true;
        }
    }
    parse_cookies(headers)
        .get(ADMIN_COOKIE)
        .map_or(false, |v| ct_eq(v, token))
}

/// Middleware gating admin-only routes.
pub async fn require_admin(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if is_admin(req.headers(), req.uri().query(), &state.admin_token) {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            "admin access required — open the admin link with ?token=",
        )
            .into_response()
    }
}

/// Middleware recording the anonymous session cookie on every request so
/// public users are accounted for.
pub async fn track_session(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if let Some(id) = parse_cookies(req.headers()).get(ANON_COOKIE) {
        state.sessions.touch(id);
    }
    next.run(req).await
}

#[derive(Debug, Serialize)]
pub struct WhoAmI {
    pub admin: bool,
}

/// Public endpoint the page calls on load to decide whether to show the
/// admin-only controls. The real enforcement is server-side regardless.
pub async fn whoami(State(state): State<AppState>, headers: HeaderMap, uri: Uri) -> Json<WhoAmI> {
    Json(WhoAmI {
        admin: is_admin(&headers, uri.query(), &state.admin_token),
    })
}

/// Tracks distinct anonymous public sessions by their cookie id.
#[derive(Default)]
pub struct SessionRegistry {
    seen: Mutex<HashMap<String, Instant>>,
}

impl SessionRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record that an anonymous session is alive right now.
    pub fn touch(&self, id: &str) {
        if id.is_empty() {
            return;
        }
        self.seen.lock().insert(id.to_string(), Instant::now());
    }

    /// Count distinct sessions seen within `window`, pruning older ones.
    pub fn active(&self, window: Duration) -> usize {
        let cutoff = Instant::now() - window;
        let mut g = self.seen.lock();
        g.retain(|_, &mut t| t >= cutoff);
        g.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn headers_with_cookie(cookie: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_str(cookie).unwrap());
        h
    }

    #[test]
    fn ct_eq_matches_and_rejects() {
        assert!(ct_eq(TOKEN, TOKEN));
        assert!(!ct_eq(TOKEN, "0123456789abcdef0123456789abcde0"));
        assert!(!ct_eq(TOKEN, "short"));
    }

    #[test]
    fn parses_multiple_cookies() {
        let c = parse_cookies(&headers_with_cookie(&format!(
            "{ANON_COOKIE}=abc; {ADMIN_COOKIE}={TOKEN}"
        )));
        assert_eq!(c.get(ANON_COOKIE).map(String::as_str), Some("abc"));
        assert_eq!(c.get(ADMIN_COOKIE).map(String::as_str), Some(TOKEN));
    }

    #[test]
    fn admin_via_query_token() {
        let h = HeaderMap::new();
        assert!(is_admin(&h, Some(&format!("token={TOKEN}")), TOKEN));
        assert!(is_admin(
            &h,
            Some(&format!("profile=low&token={TOKEN}&t=1")),
            TOKEN
        ));
        assert!(!is_admin(&h, Some("token=wrong"), TOKEN));
    }

    #[test]
    fn admin_via_cookie() {
        let h = headers_with_cookie(&format!("{ADMIN_COOKIE}={TOKEN}"));
        assert!(is_admin(&h, None, TOKEN));
        let bad = headers_with_cookie(&format!("{ADMIN_COOKIE}=nope"));
        assert!(!is_admin(&bad, None, TOKEN));
    }

    #[test]
    fn anonymous_is_not_admin() {
        assert!(!is_admin(&HeaderMap::new(), None, TOKEN));
        assert!(!is_admin(
            &headers_with_cookie(&format!("{ANON_COOKIE}=anon-id")),
            Some("profile=low"),
            TOKEN
        ));
    }

    #[test]
    fn sessions_counted_and_pruned() {
        let s = SessionRegistry::default();
        s.touch("a");
        s.touch("b");
        s.touch("a"); // same session, not double counted
        assert_eq!(s.active(Duration::from_secs(60)), 2);
        // a zero window prunes everything just-seen
        assert_eq!(s.active(Duration::from_secs(0)), 0);
    }
}
