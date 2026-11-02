//! The phone as a touchpad, air mouse and keyboard, from its browser: no
//! app to install. Nook serves one page over HTTPS on the local network
//! (motion sensors only work on a secure page, so the certificate is
//! self-signed and the browser warns once), and the page sends input over
//! a WebSocket.
//!
//! - **Access:** the address and a random token travel in the QR code
//!   (`https://ip:port/#token`); the WebSocket refuses any other token, so
//!   others on the network can load the page but not drive anything. A
//!   new token is made each time Nook starts.
//! - **Input:** a thread of its own owns the platform's [`RemoteInput`]
//!   session. The first message opens it, which on GNOME asks the user
//!   once; the approval is kept as a restore token next to the
//!   certificate. Motion is coalesced, so a slow portal never lags.
//! - **Protocol** (one text message each): `m dx dy` move, `s dx dy`
//!   scroll and `se` its end, `d b` / `u b` / `c b` button down, up and
//!   click (`l`, `r`, `m`), `t text` type, `k Name` a named key, `h`
//!   hello.

use nook_platform::remote_input::{self, Button, RemoteInput};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PAGE: &str = include_str!("remote.html");
/// Tried first, so a bookmark on the phone keeps working.
const PORT: u16 = 47811;

/// Where the input side stands, for the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Access {
    /// No phone has sent anything yet.
    #[default]
    Idle,
    /// Waiting for the user to allow remote interaction.
    Asking,
    Allowed,
    /// Refused or failed; the next message from the phone asks again.
    Failed(String),
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    /// Phones connected right now.
    pub phones: usize,
    pub access: Access,
}

/// The running server; it stops with the process.
pub struct RemoteServer {
    pub url: String,
    pub status: Arc<Mutex<Status>>,
}

impl RemoteServer {
    /// Starts serving. `dir` keeps the certificate and the input approval.
    pub fn start(dir: &Path, ctx: egui::Context) -> Result<Self, String> {
        // A home network address if there is one (not a VPN's).
        let ips: Vec<Ipv4Addr> = super::localsend::interfaces()
            .into_iter()
            .map(|(ip, _)| ip)
            .collect();
        let ip = ips
            .iter()
            .find(|ip| ip.is_private())
            .or_else(|| ips.first())
            .copied()
            .ok_or("Not connected to a network")?;
        let tls = tls_config(dir, ip)?;
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, PORT))
            .or_else(|_| TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)))
            .map_err(|e| format!("Could not open a port: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let token = super::adb::random_token(16);
        let url = format!("https://{ip}:{port}/#{token}");
        let status = Arc::new(Mutex::new(Status::default()));

        let (input_tx, input_rx) = mpsc::channel();
        let st = status.clone();
        let c = ctx.clone();
        let token_file = dir.join("remote-input-token");
        spawn("nook-remote-input", move || {
            inject(&input_rx, &token_file, &st, &c)
        });

        let st = status.clone();
        spawn("nook-remote-accept", move || {
            for stream in listener.incoming().flatten() {
                let (tls, token, tx, st, ctx) = (
                    tls.clone(),
                    token.clone(),
                    input_tx.clone(),
                    st.clone(),
                    ctx.clone(),
                );
                spawn("nook-remote-conn", move || {
                    if let Err(e) = serve(stream, tls, &token, &tx, &st, &ctx) {
                        tracing::debug!("remote: {e}");
                    }
                });
            }
        });
        Ok(Self { url, status })
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(f) {
        tracing::warn!("spawning {name}: {e}");
    }
}

/// A self-signed certificate, made once and kept in `dir` so the phone's
/// browser remembers the exception.
fn tls_config(dir: &Path, ip: Ipv4Addr) -> Result<Arc<rustls::ServerConfig>, String> {
    let (cert_file, key_file) = (dir.join("remote-cert.der"), dir.join("remote-key.der"));
    let stored = std::fs::read(&cert_file)
        .ok()
        .zip(std::fs::read(&key_file).ok());
    let (cert, key) = match stored {
        Some(pair) => pair,
        None => {
            let made =
                rcgen::generate_simple_self_signed(vec![ip.to_string(), "nook.local".into()])
                    .map_err(|e| format!("certificate: {e}"))?;
            let pair = (made.cert.der().to_vec(), made.key_pair.serialize_der());
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(&cert_file, &pair.0);
            let _ = std::fs::write(&key_file, &pair.1);
            pair
        }
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
        )
        .map_err(|e| format!("certificate: {e}"))?;
    Ok(Arc::new(config))
}

type Tls = rustls::StreamOwned<rustls::ServerConnection, TcpStream>;

/// One connection: the page, or the input WebSocket.
fn serve(
    tcp: TcpStream,
    tls: Arc<rustls::ServerConfig>,
    token: &str,
    input: &Sender<Cmd>,
    status: &Mutex<Status>,
    ctx: &egui::Context,
) -> Result<(), String> {
    tcp.set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|e| e.to_string())?;
    let conn = rustls::ServerConnection::new(tls).map_err(|e| e.to_string())?;
    let mut s: Tls = rustls::StreamOwned::new(conn, tcp);
    let head = read_head(&mut s)?;
    let request = Request::parse(&head).ok_or("bad request")?;
    match request.path.split('?').next().unwrap_or("") {
        "/" | "/index.html" => respond(
            &mut s,
            "200 OK",
            "text/html; charset=utf-8",
            PAGE.as_bytes(),
        ),
        "/ws" => {
            let given = request
                .path
                .split_once("t=")
                .map(|(_, t)| t.split('&').next().unwrap_or(""));
            let Some(key) = request.header("sec-websocket-key") else {
                return respond(&mut s, "400 Bad Request", "text/plain", b"WebSocket only");
            };
            if given != Some(token) {
                return respond(
                    &mut s,
                    "403 Forbidden",
                    "text/plain",
                    b"Scan the code again",
                );
            }
            let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
            write!(
                s,
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            )
            .and_then(|()| s.flush())
            .map_err(|e| e.to_string())?;
            // The phone pings every few seconds; silence means it's gone.
            s.sock
                .set_read_timeout(Some(Duration::from_secs(20)))
                .map_err(|e| e.to_string())?;
            let ws = tungstenite::WebSocket::from_raw_socket(
                s,
                tungstenite::protocol::Role::Server,
                None,
            );
            phone(ws, input, status, ctx);
            Ok(())
        }
        _ => respond(&mut s, "404 Not Found", "text/plain", b"Not found"),
    }
}

/// Reads up to the blank line ending the request head.
fn read_head(s: &mut Tls) -> Result<String, String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        if buf.len() > 16 * 1024 {
            return Err("request head too long".into());
        }
        let n = s.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("closed".into());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn respond(s: &mut Tls, status: &str, kind: &str, body: &[u8]) -> Result<(), String> {
    write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .and_then(|()| s.write_all(body))
    .and_then(|()| s.flush())
    .map_err(|e| e.to_string())?;
    s.conn.send_close_notify();
    let _ = s.flush();
    Ok(())
}

struct Request {
    path: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn parse(head: &str) -> Option<Self> {
        let mut lines = head.lines();
        let mut first = lines.next()?.split_whitespace();
        if first.next()? != "GET" {
            return None;
        }
        let path = first.next()?.to_owned();
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
            .collect();
        Some(Self { path, headers })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Reads one phone's messages until it leaves.
fn phone(
    mut ws: tungstenite::WebSocket<Tls>,
    input: &Sender<Cmd>,
    status: &Mutex<Status>,
    ctx: &egui::Context,
) {
    let bump = |d: isize| {
        if let Ok(mut s) = status.lock() {
            s.phones = s.phones.saturating_add_signed(d);
        }
        ctx.request_repaint();
    };
    bump(1);
    while let Ok(msg) = ws.read() {
        let tungstenite::Message::Text(text) = msg else {
            continue;
        };
        if let Some(cmd) = Cmd::parse(text.as_str())
            && input.send(cmd).is_err()
        {
            break;
        }
    }
    bump(-1);
}

/// One input message from the phone.
#[derive(Debug, PartialEq)]
enum Cmd {
    Hello,
    Move(f64, f64),
    Scroll(f64, f64),
    ScrollEnd,
    Button(Button, Press),
    Type(String),
    Key(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Press {
    Down,
    Up,
    Click,
}

impl Cmd {
    fn parse(text: &str) -> Option<Self> {
        let (op, rest) = text.split_once(' ').unwrap_or((text, ""));
        let pair = || {
            let mut it = rest.split_whitespace().map(|v| v.parse::<f64>().ok());
            let (x, y) = (it.next()??, it.next()??);
            (x.is_finite() && y.is_finite())
                .then(|| (x.clamp(-500.0, 500.0), y.clamp(-500.0, 500.0)))
        };
        let button = || match rest.trim() {
            "l" => Some(Button::Left),
            "r" => Some(Button::Right),
            "m" => Some(Button::Middle),
            _ => None,
        };
        Some(match op {
            "h" => Cmd::Hello,
            "m" => pair().map(|(x, y)| Cmd::Move(x, y))?,
            "s" => pair().map(|(x, y)| Cmd::Scroll(x, y))?,
            "se" => Cmd::ScrollEnd,
            "d" => Cmd::Button(button()?, Press::Down),
            "u" => Cmd::Button(button()?, Press::Up),
            "c" => Cmd::Button(button()?, Press::Click),
            "t" if !rest.is_empty() => Cmd::Type(rest.chars().take(500).collect()),
            "k" => Cmd::Key(rest.trim().to_owned()),
            _ => return None,
        })
    }
}

/// Owns the input session and plays the phones' messages.
fn inject(rx: &Receiver<Cmd>, token_file: &PathBuf, status: &Mutex<Status>, ctx: &egui::Context) {
    let set = |a: Access| {
        if let Ok(mut s) = status.lock() {
            s.access = a;
        }
        ctx.request_repaint();
    };
    let mut session: Option<RemoteInput> = None;
    loop {
        let cmd = match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if session.is_none() {
            set(Access::Asking);
            let saved = std::fs::read_to_string(token_file).ok();
            match RemoteInput::start(saved.as_deref().map(str::trim)) {
                Ok(Some((s, token))) => {
                    if let Some(t) = token {
                        let _ = std::fs::write(token_file, t);
                    }
                    session = Some(s);
                    set(Access::Allowed);
                }
                Ok(None) => set(Access::Failed("Remote control was not allowed".into())),
                Err(e) => set(Access::Failed(e.to_string())),
            }
            // What queued up while the question was open is stale.
            while rx.try_recv().is_ok() {}
            continue;
        }
        let Some(s) = &session else {
            continue;
        };
        let result = match cmd {
            Cmd::Hello => Ok(()),
            Cmd::Move(mut dx, mut dy) => {
                // Fold in whatever motion has queued behind this one.
                let mut rest = Vec::new();
                while let Ok(next) = rx.try_recv() {
                    match next {
                        Cmd::Move(x, y) => {
                            dx += x;
                            dy += y;
                        }
                        other => {
                            rest.push(other);
                            break;
                        }
                    }
                }
                let r = s.motion(dx, dy);
                for other in rest {
                    let _ = play(s, other);
                }
                r
            }
            other => play(s, other),
        };
        if let Err(e) = result {
            tracing::warn!("remote input: {e}");
            session = None;
            set(Access::Failed(e.to_string()));
        }
    }
}

fn play(s: &RemoteInput, cmd: Cmd) -> nook_platform::Result<()> {
    match cmd {
        Cmd::Hello => Ok(()),
        Cmd::Move(dx, dy) => s.motion(dx, dy),
        Cmd::Scroll(dx, dy) => s.scroll(dx, dy, false),
        Cmd::ScrollEnd => s.scroll(0.0, 0.0, true),
        Cmd::Button(b, Press::Down) => s.button(b, true),
        Cmd::Button(b, Press::Up) => s.button(b, false),
        Cmd::Button(b, Press::Click) => s.button(b, true).and_then(|()| s.button(b, false)),
        Cmd::Type(text) => {
            for c in text.chars() {
                let sym = remote_input::keysym_for_char(c);
                s.key(sym, true)?;
                s.key(sym, false)?;
            }
            Ok(())
        }
        Cmd::Key(name) => match remote_input::keysym_for_name(&name) {
            Some(sym) => s.key(sym, true).and_then(|()| s.key(sym, false)),
            None => Ok(()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_parse() {
        assert_eq!(Cmd::parse("m 3.5 -2"), Some(Cmd::Move(3.5, -2.0)));
        assert_eq!(Cmd::parse("m 9999 0"), Some(Cmd::Move(500.0, 0.0)));
        assert_eq!(Cmd::parse("m NaN 0"), None);
        assert_eq!(
            Cmd::parse("c r"),
            Some(Cmd::Button(Button::Right, Press::Click))
        );
        assert_eq!(Cmd::parse("d x"), None);
        assert_eq!(Cmd::parse("t שלום"), Some(Cmd::Type("שלום".into())));
        assert_eq!(Cmd::parse("t "), None);
        assert_eq!(Cmd::parse("k Enter"), Some(Cmd::Key("Enter".into())));
        assert_eq!(Cmd::parse("se"), Some(Cmd::ScrollEnd));
        assert_eq!(Cmd::parse("rm -rf"), None);
    }

    #[test]
    fn requests_parse() {
        let r =
            Request::parse("GET /ws?t=abc HTTP/1.1\r\nHost: x\r\nSec-WebSocket-Key: k==\r\n\r\n")
                .unwrap();
        assert_eq!(r.path, "/ws?t=abc");
        assert_eq!(r.header("sec-websocket-key"), Some("k=="));
        assert!(Request::parse("POST / HTTP/1.1\r\n\r\n").is_none());
    }
}
