//! A tiny HTTP/1.1 server for the external link check and retry tests: canned routes (two of
//! them missing until [`LinkServer::fix`]) on 127.0.0.1 (and
//! `[::1]` when available, so `localhost` connects at once), a log of every request with its
//! headers, and how much of a large body could be written before the client went away. No network.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// The size of the `/big` body.
pub const BIG_BODY: usize = 5 * 1024 * 1024;

/// A request as the server saw it.
#[derive(Debug, Clone)]
pub struct Request {
    pub path: String,
    /// Header names in lower case.
    pub headers: Vec<(String, String)>,
    pub at: Instant,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The host name of the `Host` header (without the port).
    pub fn host(&self) -> String {
        let host = self.header("host").unwrap_or("");
        match host.rsplit_once(':') {
            Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name.to_string(),
            _ => host.to_string(),
        }
    }
}

#[derive(Default)]
struct Shared {
    requests: Mutex<Vec<Request>>,
    /// Bytes of the `/big` body written before the client closed the connection.
    big_written: Mutex<Vec<usize>>,
    /// `/flaky-page` and `/flaky.png` answer 200 instead of 404.
    fixed: AtomicBool,
    stop: AtomicBool,
    port: std::sync::atomic::AtomicU16,
}

pub struct LinkServer {
    pub port: u16,
    shared: Arc<Shared>,
}

impl LinkServer {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("address").port();
        let shared = Arc::new(Shared::default());
        shared.port.store(port, Ordering::SeqCst);
        let mut listeners = vec![listener];
        // `localhost` may resolve to ::1 first; without a listener there a connect waits.
        if let Ok(v6) = TcpListener::bind(("::1", port)) {
            listeners.push(v6);
        }
        for listener in listeners {
            let shared = Arc::clone(&shared);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if shared.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let shared = Arc::clone(&shared);
                    thread::spawn(move || serve(stream, &shared));
                }
            });
        }
        Self { port, shared }
    }

    /// `http://127.0.0.1:<port><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// `http://<host>:<port><path>`.
    pub fn url_on(&self, host: &str, path: &str) -> String {
        format!("http://{host}:{}{path}", self.port)
    }

    pub fn requests(&self) -> Vec<Request> {
        self.shared.requests.lock().expect("log").clone()
    }

    /// How often `path` was requested.
    pub fn count(&self, path: &str) -> usize {
        self.requests().iter().filter(|request| request.path == path).count()
    }

    /// Requests per path.
    pub fn counts(&self) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        for request in self.requests() {
            *counts.entry(request.path).or_insert(0) += 1;
        }
        counts
    }

    pub fn big_written(&self) -> Vec<usize> {
        self.shared.big_written.lock().expect("log").clone()
    }

    /// From now on `/flaky-page` and `/flaky.png` answer 200 (they answer 404 before).
    pub fn fix(&self) {
        self.shared.fixed.store(true, Ordering::SeqCst);
    }
}

impl Drop for LinkServer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // Wake the accept loops so they see the flag.
        let _ = TcpStream::connect_timeout(&([127, 0, 0, 1], self.port).into(), Duration::from_millis(200));
        let _ = TcpStream::connect_timeout(
            &(std::net::Ipv6Addr::LOCALHOST, self.port).into(),
            Duration::from_millis(200),
        );
    }
}

/// `/src/css/page-styles.css`: a missing font on line 4, a missing image twice (lines 6 and 8).
pub const STYLESHEET: &str = "body { margin: 0; }\n\n/* Fonts */\n@font-face { font-family: Example; src: url(\"../fonts/missing.woff2\") format(\"woff2\"); }\n\n.hero { background: url(../img/missing.png) no-repeat; }\n.card { color: #333; }\n.hero-small { background: url( '../img/missing.png' ); }\n";

/// `/src/css/style.min.css`: one long line; `url(/img/missing-bg.png)` at the end.
pub fn minified_stylesheet() -> String {
    let mut css: String = (0..300).map(|n| format!(".r{n}{{color:#{n:03}}}")).collect();
    css.push_str(".x{background:url(/img/missing-bg.png)}");
    css
}

fn page(title: &str, body: &str) -> String {
    format!("<!DOCTYPE html><html><head><title>{title}</title></head><body>{body}</body></html>")
}

fn respond(stream: &mut TcpStream, status: &str, headers: &[(&str, &str)], body: &[u8]) {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn serve(mut stream: TcpStream, shared: &Shared) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") && bytes.len() < 64 * 1024 {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                bytes.extend_from_slice(&buffer[..read]);
                // Not HTTP (e.g. a TLS handshake): answer in plain text at once.
                if !bytes.starts_with(b"GET ") && !bytes.starts_with(b"HEAD ") && bytes.len() >= 4 {
                    respond(&mut stream, "400 Bad Request", &[], b"");
                    return;
                }
            }
        }
    }
    let text = String::from_utf8_lossy(&bytes).to_string();
    let mut lines = text.split("\r\n");
    let path = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or("")
        .to_string();
    let headers = lines
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    shared.requests.lock().expect("log").push(Request {
        path: path.clone(),
        headers,
        at: Instant::now(),
    });
    let html = [("Content-Type", "text/html; charset=utf-8")];
    match path.split('?').next().unwrap_or("") {
        "/ok" => respond(
            &mut stream,
            "200 OK",
            &html,
            page(
                "OK &amp; Alive",
                "<img src=\"/img.png\"><script src=\"/app.js\"></script><link rel=\"stylesheet\" href=\"/style.css\"><a href=\"/other\">x</a>",
            )
            .as_bytes(),
        ),
        "/404" => respond(&mut stream, "404 Not Found", &html, page("Not found", "").as_bytes()),
        "/403" => respond(&mut stream, "403 Forbidden", &html, page("Forbidden", "").as_bytes()),
        "/500" => respond(&mut stream, "500 Internal Server Error", &[], b"oops"),
        "/redirect/1" => respond(&mut stream, "301 Moved Permanently", &[("Location", "/redirect/2")], b""),
        "/redirect/2" => respond(&mut stream, "302 Found", &[("Location", "/redirect/3")], b""),
        "/redirect/3" => respond(&mut stream, "307 Temporary Redirect", &[("Location", "/final")], b""),
        "/final" => respond(&mut stream, "200 OK", &html, page("Final page", "").as_bytes()),
        "/loop" => respond(&mut stream, "302 Found", &[("Location", "/loop")], b""),
        "/slow" => {
            thread::sleep(Duration::from_secs(4));
            respond(&mut stream, "200 OK", &html, page("Slow", "").as_bytes());
        }
        "/slow-1s" => {
            thread::sleep(Duration::from_secs(1));
            respond(&mut stream, "200 OK", &html, page("Slow", "").as_bytes());
        }
        "/close" => {
            let _ = stream.shutdown(Shutdown::Both);
        }
        "/garbage" => {
            let _ = stream.write_all(b"\x00\xffNOT HTTP AT ALL\r\n\r\n\xfe\xfd");
            let _ = stream.shutdown(Shutdown::Both);
        }
        // Source pages of a site, for link texts.
        "/src/a" => {
            let port = shared.port.load(Ordering::SeqCst);
            let body = format!(
                "<p>See <a href=\"/404\">Broken  <b>page</b></a> and <a href=\"http://localhost:{port}/ok\"><img src=\"/i.png\" alt=\"Partner logo\"></a>.</p>"
            );
            respond(&mut stream, "200 OK", &html, page("Source A", &body).as_bytes());
        }
        "/src/b" => respond(&mut stream, "200 OK", &html, page("Source B", "<p>No links here.</p>").as_bytes()),
        // Stylesheets of a site, for the positions of `url()` references.
        "/src/css/page-styles.css" => respond(
            &mut stream,
            "200 OK",
            &[("Content-Type", "text/css; charset=utf-8")],
            STYLESHEET.as_bytes(),
        ),
        "/src/css/style.min.css" => respond(
            &mut stream,
            "200 OK",
            &[("Content-Type", "text/css")],
            minified_stylesheet().as_bytes(),
        ),
        // `/redirect-to?<absolute URL>`: a redirect to anywhere.
        "/redirect-to" => {
            let location = path.split_once('?').map_or("", |(_, query)| query).to_string();
            respond(&mut stream, "302 Found", &[("Location", location.as_str())], b"");
        }
        "/src/redirect" => {
            let port = shared.port.load(Ordering::SeqCst);
            let location = format!("http://localhost:{port}/src/a");
            respond(&mut stream, "302 Found", &[("Location", location.as_str())], b"");
        }
        // A small site whose page and image are missing until `fix`.
        "/site" => respond(
            &mut stream,
            "200 OK",
            &html,
            page(
                "Site",
                "<a href=\"/flaky-page\">Flaky page</a> <img src=\"/flaky.png\" alt=\"Flaky image\">",
            )
            .as_bytes(),
        ),
        "/flaky-page" if shared.fixed.load(Ordering::SeqCst) => respond(
            &mut stream,
            "200 OK",
            &html,
            page("Back  again", "<img src=\"/img.png\"><a href=\"/other\">x</a>").as_bytes(),
        ),
        "/flaky.png" if shared.fixed.load(Ordering::SeqCst) => {
            respond(&mut stream, "200 OK", &[("Content-Type", "image/png")], b"\x89PNG\r\n\x1a\n")
        }
        "/flaky-page" | "/flaky.png" => respond(&mut stream, "404 Not Found", &html, page("Not found", "").as_bytes()),
        // A shop page with cart and action links (WooCommerce, WordPress) and two plain pages.
        "/shop" => respond(
            &mut stream,
            "200 OK",
            &html,
            page(
                "Shop",
                "<a href=\"/product/red/\">Red string</a> \
                 <a href=\"/product/red/?add-to-cart=31\">Add to cart</a> \
                 <a href=\"/shop?orderby=price&amp;add-to-cart=32\">Add to cart</a> \
                 <a href=\"/cart/?remove_item=0c74b7f7&amp;_wpnonce=1a2b\">Remove</a> \
                 <a href=\"/cart/?undo_item=0c74b7f7&amp;_wpnonce=1a2b\">Undo</a> \
                 <a href=\"/wp-login.php?action=logout&amp;_wpnonce=1a2b\">Log out</a> \
                 <a href=\"/blog/?replytocom=5\">Reply</a>",
            )
            .as_bytes(),
        ),
        "/notitle" => respond(&mut stream, "200 OK", &[("Content-Type", "text/plain")], b"plain text"),
        "/big" => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {BIG_BODY}\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(head.as_bytes());
            let mut written = 0;
            // No title in the first 256 KB: the client reads exactly up to its limit, then stops.
            let chunk = vec![b' '; 16 * 1024];
            while written < BIG_BODY {
                match stream.write(&chunk[..chunk.len().min(BIG_BODY - written)]) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => written += count,
                }
            }
            shared.big_written.lock().expect("log").push(written);
        }
        _ => respond(&mut stream, "200 OK", &html, page("Other", "").as_bytes()),
    }
}
