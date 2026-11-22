//! A tiny server that speaks HTTP/1.1 and HTTPS on the same port (it looks at the first byte of a
//! connection: a TLS handshake or a request line), for the credential tests of source-page requests
//! across schemes — including an https page that redirects to http on the same host and port. On
//! 127.0.0.1 (and `[::1]` when available, so `localhost` connects at once); a log of every request
//! with its headers and whether it came over TLS. The certificate and its throwaway key are in this
//! folder (see README.md). No network.
//!
//! Routes: `/redirect-to?<absolute URL>` answers 302 to that URL; everything else a small HTML page.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

const CERTIFICATE: &[u8] = include_bytes!("localhost.cert.der");
/// A throwaway key for local tests only (README.md).
const KEY: &[u8] = include_bytes!("localhost.key.der");

/// A request as the server saw it.
#[derive(Debug, Clone)]
pub struct Request {
    pub tls: bool,
    pub path: String,
    /// Header names in lower case.
    pub headers: Vec<(String, String)>,
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
    stop: AtomicBool,
}

pub struct DualServer {
    pub port: u16,
    shared: Arc<Shared>,
}

impl DualServer {
    pub fn start() -> Self {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(CERTIFICATE.to_vec())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KEY.to_vec())),
            )
            .expect("certificate");
        let config = Arc::new(config);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("address").port();
        let shared = Arc::new(Shared::default());
        let mut listeners = vec![listener];
        if let Ok(v6) = TcpListener::bind(("::1", port)) {
            listeners.push(v6);
        }
        for listener in listeners {
            let shared = Arc::clone(&shared);
            let config = Arc::clone(&config);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if shared.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let shared = Arc::clone(&shared);
                    let config = Arc::clone(&config);
                    thread::spawn(move || serve(stream, &shared, config));
                }
            });
        }
        Self { port, shared }
    }

    /// `<scheme>://<host>:<port><path>`.
    pub fn url(&self, scheme: &str, host: &str, path: &str) -> String {
        format!("{scheme}://{host}:{}{path}", self.port)
    }

    pub fn requests(&self) -> Vec<Request> {
        self.shared.requests.lock().expect("log").clone()
    }
}

impl Drop for DualServer {
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

fn serve(stream: TcpStream, shared: &Shared, config: Arc<rustls::ServerConfig>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut first = [0u8; 1];
    // 0x16: a TLS handshake record.
    let tls = matches!(stream.peek(&mut first), Ok(1) if first[0] == 0x16);
    if tls {
        let Ok(connection) = rustls::ServerConnection::new(config) else {
            return;
        };
        let mut stream = rustls::StreamOwned::new(connection, stream);
        handle(&mut stream, true, shared);
        stream.conn.send_close_notify();
        let _ = stream.flush();
    } else {
        let mut stream = stream;
        handle(&mut stream, false, shared);
    }
}

fn handle(stream: &mut (impl Read + Write), tls: bool, shared: &Shared) {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") && bytes.len() < 64 * 1024 {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => bytes.extend_from_slice(&buffer[..read]),
        }
    }
    if bytes.is_empty() {
        // A wake-up connect, or a client that gave up during the handshake.
        return;
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
        tls,
        path: path.clone(),
        headers,
    });
    let (status, location, body) = match path.split_once('?') {
        Some(("/redirect-to", target)) => ("302 Found", Some(target.to_string()), String::new()),
        _ => (
            "200 OK",
            None,
            "<!DOCTYPE html><html><head><title>Page</title></head><body><p>A page.</p></body></html>".to_string(),
        ),
    };
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(location) = location {
        head.push_str(&format!("Location: {location}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}
