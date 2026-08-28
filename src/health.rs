//! Health endpoint mirroring upstream healthz.py.
//!
//! Bind policy: HEALTH_HOST forces the address family (an IPv4 literal binds
//! AF_INET, so IPv6 clients are refused — the suite checks this); without it,
//! dual-stack IPv6 is tried first with an IPv4 fallback.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use socket2::{Domain, Socket, Type};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::logger;

const K8S_CONTACT_THRESHOLD_SECONDS: u64 = 60;

struct Health {
    ready: AtomicBool,
    /// ms since `start` of the last successful Kubernetes contact.
    last_contact_ms: AtomicU64,
    start: Instant,
    watchers: Mutex<Vec<Arc<AtomicBool>>>,
}

static HEALTH: OnceLock<Health> = OnceLock::new();

fn health() -> &'static Health {
    HEALTH.get_or_init(|| Health {
        ready: AtomicBool::new(false),
        last_contact_ms: AtomicU64::new(0),
        start: Instant::now(),
        watchers: Mutex::new(Vec::new()),
    })
}

pub fn mark_ready() {
    health().ready.store(true, Ordering::Relaxed);
}

pub fn update_k8s_contact() {
    let h = health();
    h.last_contact_ms
        .store(h.start.elapsed().as_millis() as u64, Ordering::Relaxed);
}

pub fn register_watchers(flags: Vec<Arc<AtomicBool>>) {
    *health().watchers.lock().unwrap() = flags;
}

fn status_body() -> (u16, &'static str, String) {
    let h = health();
    if !h.ready.load(Ordering::Relaxed) {
        return (503, "Service Unavailable", "NOT READY".into());
    }
    let now_ms = h.start.elapsed().as_millis() as u64;
    let last = h.last_contact_ms.load(Ordering::Relaxed);
    if now_ms.saturating_sub(last) > K8S_CONTACT_THRESHOLD_SECONDS * 1000 {
        return (503, "Service Unavailable", "NOT LIVE (K8s contact lost)".into());
    }
    let watchers = h.watchers.lock().unwrap();
    if !watchers.is_empty() && watchers.iter().any(|w| !w.load(Ordering::Relaxed)) {
        return (503, "Service Unavailable", "NOT LIVE (watcher thread died)".into());
    }
    (200, "OK", "OK".into())
}

fn bind_listener(port: u16) -> std::io::Result<std::net::TcpListener> {
    let health_host = std::env::var("HEALTH_HOST").ok().filter(|h| !h.is_empty());

    let bind = |domain: Domain, addr: SocketAddr| -> std::io::Result<std::net::TcpListener> {
        let sock = Socket::new(domain, Type::STREAM, None)?;
        let _ = sock.set_reuse_address(true);
        if domain == Domain::IPV6 {
            let _ = sock.set_only_v6(false); // dual-stack, like Python's AF_INET6 bind
        }
        sock.bind(&addr.into())?;
        sock.listen(128)?;
        let l: std::net::TcpListener = sock.into();
        l.set_nonblocking(true)?;
        Ok(l)
    };

    match health_host {
        Some(host) => {
            let addr: SocketAddr = match host.parse::<IpAddr>() {
                Ok(ip) => SocketAddr::new(ip, port),
                // Not an IP literal: resolve it, preferring IPv6 like upstream.
                Err(_) => {
                    let mut addrs: Vec<SocketAddr> = (host.as_str(), port)
                        .to_socket_addrs()
                        .map(|it| it.collect())
                        .unwrap_or_default();
                    addrs.sort_by_key(|a| if a.is_ipv6() { 0 } else { 1 });
                    addrs.into_iter().next().ok_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::Other, "cannot resolve HEALTH_HOST")
                    })?
                }
            };
            let domain = if addr.is_ipv6() { Domain::IPV6 } else { Domain::IPV4 };
            bind(domain, addr)
        }
        None => match bind(Domain::IPV6, "[::]:0".parse::<SocketAddr>().map(|mut a| { a.set_port(port); a }).unwrap()) {
            Ok(l) => Ok(l),
            Err(_) => {
                logger::warning("IPv6 not available, falling back to IPv4 for the health server");
                bind(Domain::IPV4, SocketAddr::new("0.0.0.0".parse().unwrap(), port))
            }
        },
    }
}

async fn handle(mut stream: tokio::net::TcpStream) {
    let mut buf = [0u8; 1024];
    let n = match stream.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("/");

    let (code, reason, body) = if path == "/healthz" {
        status_body()
    } else {
        (404, "Not Found", "Not Found".into())
    };
    let resp = format!(
        "HTTP/1.0 {} {}\r\nServer: HealthHTTP/1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        code,
        reason,
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Start the health server in a background task.
pub fn start_health_server() {
    let port: u16 = std::env::var("HEALTH_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);

    // Touch the state so the contact timestamp starts "now", like upstream.
    update_k8s_contact();

    let listener = match bind_listener(port) {
        Ok(l) => l,
        Err(e) => {
            logger::error(&format!("Failed to start health server: {}", e));
            return;
        }
    };

    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(l) => l,
            Err(e) => {
                logger::error(&format!("Failed to start health server: {}", e));
                return;
            }
        };
        logger::info(&format!("Starting health server on port {}", port));
        loop {
            if let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(handle(stream));
            }
        }
    });
}
