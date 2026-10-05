//! Who may talk to the Studio.
//!
//! The Studio listens on the loopback interface only, but that alone does
//! not keep others out: a web page open in the same browser can send it
//! requests (cross-site request forgery), a page whose own domain name is
//! made to resolve to 127.0.0.1 can read the answers too (DNS rebinding),
//! and other users of the machine can reach the port. The Studio edits
//! files, writes generated code and runs compilers, so every request must
//!
//! * address it by a loopback name (`Host`: `127.0.0.1`, `localhost`,
//!   `[::1]`),
//! * come from the Studio's own page when a browser sends it (`Origin`),
//! * carry this launch's token: the cookie the launch link sets, or an
//!   `X-OpenLustre-Token` header (scripts, tests, other front ends).
//!
//! The launch link is `http://127.0.0.1:<port>/?token=<token>`. Opening it
//! sets the cookie (`HttpOnly`, `SameSite=Strict`: no other site's requests
//! carry it, and the page's scripts cannot read it) and redirects to `/`, so
//! the token leaves the address bar and the history. `/api/health` answers
//! without a token, for readiness checks.

/// The request header that carries the token for clients without the
/// cookie.
pub const TOKEN_HEADER: &str = "x-openlustre-token";

/// Set to fix the token instead of drawing a random one (scripts and tests
/// that start the Studio and then call its API).
pub const TOKEN_ENV: &str = "OPENLUSTRE_STUDIO_TOKEN";

/// The loopback names a request may address the Studio by.
const LOOPBACK_NAMES: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

/// This launch's access rule: its token and the port it serves on.
pub struct Access {
    token: String,
    port: u16,
}

/// What to do with a request.
pub enum Verdict {
    /// Serve it.
    Allow,
    /// The launch link: set the cookie (this header value) and go to `/`.
    Enter(String),
    /// Refuse it with this status, content type and body.
    Deny(u16, &'static str, Vec<u8>),
}

/// The token for this launch: [`TOKEN_ENV`] when set, otherwise 128 random
/// bits from the operating system, as hex.
pub fn launch_token() -> Result<String, String> {
    match std::env::var(TOKEN_ENV) {
        Ok(t) if !t.is_empty() => {
            let ok = t.len() >= 16 && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
            if ok {
                Ok(t)
            } else {
                Err(format!("{TOKEN_ENV} must be at least 16 letters, digits, '-' or '_'"))
            }
        }
        _ => random_token(),
    }
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| format!("no random numbers from the operating system: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl Access {
    pub fn new(token: String, port: u16) -> Self {
        Access { token, port }
    }

    /// The link that opens the Studio in a browser.
    pub fn launch_url(&self) -> String {
        format!("http://127.0.0.1:{}/?token={}", self.port, self.token)
    }

    /// Cookies are shared by every port of a host, so the name carries the
    /// port: two Studios side by side keep their own.
    fn cookie_name(&self) -> String {
        format!("openlustre_studio_{}", self.port)
    }

    /// Decide on a request from its method, path (without the query), query
    /// and header block.
    pub fn check(&self, method: &str, path: &str, query: &str, headers: &str) -> Verdict {
        match header(headers, "host") {
            Some(host) if is_loopback_host(host) => {}
            _ => return deny_text("This OpenLustre Studio only answers requests addressed to 127.0.0.1 or localhost."),
        }
        if let Some(origin) = header(headers, "origin") {
            if !self.is_own_origin(origin) {
                return deny_text("This OpenLustre Studio refuses requests from other web pages.");
            }
        }
        if path == "/api/health" {
            return Verdict::Allow;
        }
        let page = method == "GET" && (path == "/" || path == "/index.html");
        let link = if page { query_value(query, "token") } else { None };
        if link.is_some_and(|t| same(t, &self.token)) {
            return Verdict::Enter(format!(
                "{}={}; Path=/; HttpOnly; SameSite=Strict",
                self.cookie_name(),
                self.token
            ));
        }
        let presented = header(headers, TOKEN_HEADER).or_else(|| cookie(headers, &self.cookie_name()));
        if presented.is_some_and(|t| same(t, &self.token)) {
            Verdict::Allow
        } else if link.is_some() {
            deny_page(STALE_LINK)
        } else if page {
            deny_page(NO_TOKEN)
        } else {
            Verdict::Deny(
                403,
                "application/json",
                super::json_error(
                    "this request needs the Studio's launch token: open the Studio from the link \
                     `openlustre studio serve` printed, or send the X-OpenLustre-Token header",
                )
                .into_bytes(),
            )
        }
    }

    fn is_own_origin(&self, origin: &str) -> bool {
        LOOPBACK_NAMES
            .iter()
            .any(|name| origin.eq_ignore_ascii_case(&format!("http://{name}:{}", self.port)))
    }
}

/// The value of header `name` (case-insensitive) in a header block whose
/// first line is the request line.
fn header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn cookie<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    header(headers, "cookie")?.split(';').find_map(|pair| {
        let (key, value) = pair.trim().split_once('=')?;
        (key == name).then_some(value)
    })
}

fn query_value<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| {
        let (key, value) = kv.split_once('=')?;
        (key == name).then_some(value)
    })
}

/// `Host` names a loopback address (any port).
fn is_loopback_host(host: &str) -> bool {
    let name = if host.starts_with('[') {
        host.split_inclusive(']').next().unwrap_or(host)
    } else {
        host.split(':').next().unwrap_or(host)
    };
    LOOPBACK_NAMES.iter().any(|n| name.eq_ignore_ascii_case(n))
}

/// Compare without stopping at the first difference, so the time taken does
/// not tell how much of a guess was right.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn deny_text(message: &str) -> Verdict {
    Verdict::Deny(403, "text/plain; charset=utf-8", message.as_bytes().to_vec())
}

const NO_TOKEN: &str = "This page needs the Studio's launch link. Open the Studio from its \
    shortcut (Start Menu, Applications, the desktop) or with <code>openlustre studio launch</code>; \
    with <code>openlustre studio serve</code>, open the link it printed, which ends in \
    <code>?token=…</code>. The link keeps other web pages from using the Studio.";

const STALE_LINK: &str = "This link is from an earlier run of the Studio. Open the Studio again from \
    its shortcut or with <code>openlustre studio launch</code>, or use the link \
    <code>openlustre studio serve</code> printed this time.";

fn deny_page(message: &str) -> Verdict {
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>OpenLustre Studio: open it from its link</title>\
         <style>body{{font:15px/1.5 system-ui,sans-serif;max-width:36em;margin:4em auto;padding:0 1em;color:#222}}\
         code{{background:#f2f2f2;padding:0 .25em}}</style></head>\
         <body><h1>OpenLustre Studio</h1><p>{message}</p></body></html>"
    );
    Verdict::Deny(403, "text/html; charset=utf-8", html.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "0123456789abcdef0123456789abcdef";

    fn access() -> Access {
        Access::new(T.into(), 8471)
    }

    fn req(method_path: &str, extra: &str) -> String {
        format!("{method_path} HTTP/1.1\r\n{extra}")
    }

    fn verdict(method: &str, path: &str, query: &str, extra: &str) -> (u16, String) {
        match access().check(method, path, query, &req(&format!("{method} {path}"), extra)) {
            Verdict::Allow => (200, String::new()),
            Verdict::Enter(c) => (303, c),
            Verdict::Deny(s, _, body) => (s, String::from_utf8(body).unwrap()),
        }
    }

    #[test]
    fn the_token_in_a_header_or_the_cookie_lets_a_request_in() {
        let header = format!("Host: 127.0.0.1:8471\r\nX-OpenLustre-Token: {T}");
        assert_eq!(verdict("POST", "/api/edit/undo", "", &header).0, 200);
        let cookie = format!("Host: 127.0.0.1:8471\r\nCookie: other=1; openlustre_studio_8471={T}");
        assert_eq!(verdict("GET", "/api/inspect", "", &cookie).0, 200);
        assert_eq!(verdict("GET", "/", "", &cookie).0, 200);
    }

    #[test]
    fn without_the_token_the_api_and_the_page_are_refused() {
        assert_eq!(verdict("POST", "/api/clite/compile", "", "Host: 127.0.0.1").0, 403);
        let (status, page) = verdict("GET", "/", "", "Host: 127.0.0.1");
        assert_eq!(status, 403);
        assert!(page.contains("launch link") && !page.contains("diagram-status"));
        let wrong = "Host: 127.0.0.1\r\nX-OpenLustre-Token: 0123456789abcdef0123456789abcdee";
        assert_eq!(verdict("GET", "/api/inspect", "", wrong).0, 403);
        // A cookie for another Studio's port does not count.
        let other = format!("Host: 127.0.0.1\r\nCookie: openlustre_studio_8472={T}");
        assert_eq!(verdict("GET", "/api/inspect", "", &other).0, 403);
    }

    #[test]
    fn the_launch_link_sets_a_strict_cookie_and_a_stale_one_is_explained() {
        let (status, cookie) = verdict("GET", "/", &format!("token={T}"), "Host: 127.0.0.1:8471");
        assert_eq!(status, 303);
        assert_eq!(cookie, format!("openlustre_studio_8471={T}; Path=/; HttpOnly; SameSite=Strict"));
        let (status, page) = verdict("GET", "/", "token=0123456789abcdef", "Host: 127.0.0.1:8471");
        assert_eq!(status, 403);
        assert!(page.contains("earlier run"));
    }

    #[test]
    fn other_hosts_and_other_origins_are_refused_even_with_the_token() {
        let t = format!("X-OpenLustre-Token: {T}");
        // DNS rebinding: the browser names the attacker's host.
        assert_eq!(verdict("GET", "/api/inspect", "", &format!("Host: evil.example:8471\r\n{t}")).0, 403);
        assert_eq!(verdict("GET", "/api/inspect", "", &t).0, 403, "no Host at all");
        // Another page, another local server, a sandboxed frame.
        for origin in ["https://evil.example", "http://127.0.0.1:3000", "null"] {
            let h = format!("Host: 127.0.0.1:8471\r\nOrigin: {origin}\r\n{t}");
            assert_eq!(verdict("POST", "/api/edit/undo", "", &h).0, 403, "{origin}");
        }
        for origin in ["http://127.0.0.1:8471", "http://localhost:8471", "http://[::1]:8471"] {
            let h = format!("Host: localhost:8471\r\nOrigin: {origin}\r\n{t}");
            assert_eq!(verdict("POST", "/api/edit/undo", "", &h).0, 200, "{origin}");
        }
    }

    #[test]
    fn health_needs_no_token_but_still_a_loopback_host() {
        assert_eq!(verdict("GET", "/api/health", "", "Host: 127.0.0.1").0, 200);
        assert_eq!(verdict("GET", "/api/health", "", "Host: evil.example").0, 403);
    }

    #[test]
    fn an_old_link_still_opens_the_page_once_the_cookie_is_set() {
        let h = format!("Host: 127.0.0.1:8471\r\nCookie: openlustre_studio_8471={T}");
        assert_eq!(verdict("GET", "/", "token=0123456789abcdef", &h).0, 200);
    }

    #[test]
    fn tokens_are_128_random_bits() {
        let (a, b) = (random_token().unwrap(), random_token().unwrap());
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
