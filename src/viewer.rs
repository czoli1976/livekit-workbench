//! A tiny local web server for the browser viewer page.
//!
//! The page is static. The LiveKit URL and the access token travel in the URL
//! fragment (after `#`), which browsers never send to a server.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const PAGE: &str = include_str!("../assets/viewer.html");

/// A running viewer server. Dropping it stops the server.
pub struct ViewerServer {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ViewerServer {
    /// Serve the page on a free localhost port.
    pub fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new().name("viewer-http".into()).spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let request = String::from_utf8_lossy(&buf[..n]);
                        let path = request.split_whitespace().nth(1).unwrap_or("/");
                        let response = if path == "/" || path.starts_with("/?") || path == "/index.html" {
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
                                PAGE.len(),
                                PAGE
                            )
                        } else {
                            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                        };
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        })?;
        Ok(Self {
            port,
            stop,
            thread: Some(thread),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The link a browser opens to join a room as a viewer.
    pub fn link(&self, livekit_url: &str, token: &str, adaptive: bool) -> String {
        let mut l = link(self.port, livekit_url, token);
        if adaptive {
            l.push_str("&adaptive=1");
        }
        l
    }
}

impl Drop for ViewerServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn link(port: u16, livekit_url: &str, token: &str) -> String {
    format!(
        "http://127.0.0.1:{port}/#url={}&token={}",
        encode(livekit_url),
        encode(token)
    )
}

fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Open a URL in the default browser, or in a named one where the OS allows it.
pub fn open_url(url: &str, browser: Option<&str>) -> std::io::Result<()> {
    launch(url, browser).status().and_then(ok_status)
}

#[cfg(target_os = "macos")]
fn launch(url: &str, browser: Option<&str>) -> std::process::Command {
    let mut cmd = std::process::Command::new("open");
    if let Some(app) = browser {
        cmd.args(["-a", app]);
    }
    cmd.arg(url);
    cmd
}

#[cfg(target_os = "windows")]
fn launch(url: &str, browser: Option<&str>) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    // `start` goes through cmd.exe, so the URL must be quoted: it contains `&`.
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/C", "start", "\"\""]);
    if let Some(b) = browser {
        cmd.arg(b);
    }
    cmd.raw_arg(format!("\"{url}\""));
    cmd
}

#[cfg(all(unix, not(target_os = "macos")))]
fn launch(url: &str, browser: Option<&str>) -> std::process::Command {
    let mut cmd = std::process::Command::new(browser.unwrap_or("xdg-open"));
    cmd.arg(url);
    cmd
}

fn ok_status(s: std::process::ExitStatus) -> std::io::Result<()> {
    if s.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "browser launcher exited with {s}"
        )))
    }
}

/// The command name for Chrome on this OS, for "Open in Chrome".
pub fn chrome_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Google Chrome"
    } else if cfg!(target_os = "windows") {
        "chrome"
    } else {
        "google-chrome"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_escapes_token() {
        let l = link(1234, "ws://127.0.0.1:7880", "a.b+c/d=");
        assert_eq!(
            l,
            "http://127.0.0.1:1234/#url=ws%3A%2F%2F127.0.0.1%3A7880&token=a.b%2Bc%2Fd%3D"
        );
    }

    #[test]
    fn serves_page_and_stops() {
        let server = ViewerServer::start().unwrap();
        let port = server.port();
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut body = String::new();
        s.read_to_string(&mut body).unwrap();
        assert!(body.starts_with("HTTP/1.1 200"));
        assert!(body.contains("Workbench for LiveKit"));
        drop(server);
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    }
}
