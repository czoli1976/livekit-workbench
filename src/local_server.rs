//! The local dev server: find `livekit-server`, start it with a generated config,
//! capture its log, and read its health from the Prometheus endpoint and the room API.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};

use crate::profiles::{LocalServerOptions, LOCAL_DEV_KEY, LOCAL_DEV_SECRET};

pub const PROMETHEUS_PORT_OFFSET: u16 = 1000; // 7880 -> 8880
const LOG_LINES: usize = 500;

#[derive(Clone, Debug, PartialEq, Default)]
pub enum ServerState {
    #[default]
    Stopped,
    Starting,
    /// Started and owned by the app.
    Running,
    /// Something else already listens on the port; the app uses it but did not start it.
    External,
    Failed(String),
}

#[derive(Clone, Debug, Default)]
pub struct LogLine {
    pub level: String,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct ServerStats {
    pub version: String,
    pub started_at: Option<Instant>,
    pub cpu_percent: Option<f64>,
    pub memory_mb: Option<f64>,
    pub rooms: Option<u64>,
    pub participants: Option<u64>,
    pub packets_in_per_s: Option<f64>,
    pub packets_out_per_s: Option<f64>,
    pub mbps_in: Option<f64>,
    pub mbps_out: Option<f64>,
    pub nack_per_s: Option<f64>,
    /// Upstream FlexFEC since the server started: FEC packets received,
    /// media packets recovered from them, FEC packets it could not use.
    pub fec_received: Option<u64>,
    pub fec_recovered: Option<u64>,
    pub fec_discarded: Option<u64>,
    /// NACKs since the server started.
    pub nack_total: Option<u64>,
    pub room_names: Vec<String>,
    pub participant_names: Vec<String>,
    pub warnings: u64,
    pub errors: u64,
    pub log: VecDeque<LogLine>,
}

#[derive(Default)]
struct Inner {
    state: ServerState,
    stats: ServerStats,
    child: Option<Child>,
    prev_metrics: Option<(Instant, HashMap<String, f64>)>,
}

/// Handle to the local server. Cheap to clone.
#[derive(Clone, Default)]
pub struct LocalServer {
    inner: Arc<Mutex<Inner>>,
}

/// Find the server binary: the profile's path, PATH, then common install places.
pub fn find_binary(configured: &str) -> Option<PathBuf> {
    if !configured.trim().is_empty() {
        let p = PathBuf::from(configured.trim());
        return p.is_file().then_some(p);
    }
    let exe = if cfg!(windows) {
        "livekit-server.exe"
    } else {
        "livekit-server"
    };
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).map(|d| d.join(exe)).collect())
        .unwrap_or_default();
    // Where the setup scripts install it.
    if let Some(d) = directories::ProjectDirs::from("io", "czoli1976", "livekit-workbench") {
        candidates.push(d.data_dir().join("bin").join(exe));
    }
    for extra in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        candidates.push(PathBuf::from(extra).join(exe));
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".local/bin").join(exe));
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn config_yaml(opts: &LocalServerOptions) -> String {
    format!(
        "port: {port}\n\
         rtc:\n  tcp_port: {tcp}\n  port_range_start: 50000\n  port_range_end: 60000\n  use_external_ip: false\n  flexfec:\n    upstream_enabled: {fec}\n\
         keys:\n  {key}: {secret}\n\
         prometheus:\n  port: {prom}\n\
         logging:\n  level: info\n",
        port = opts.port,
        tcp = opts.port + 1,
        fec = opts.flexfec_upstream,
        key = LOCAL_DEV_KEY,
        secret = LOCAL_DEV_SECRET,
        prom = opts.port + PROMETHEUS_PORT_OFFSET,
    )
}

/// Where the app notes the process id of the server it started.
fn pid_file() -> Option<PathBuf> {
    directories::ProjectDirs::from("io", "czoli1976", "livekit-workbench")
        .map(|d| d.data_dir().join("livekit-server.pid"))
}

fn remove_pid_file() {
    if let Some(p) = pid_file() {
        let _ = std::fs::remove_file(p);
    }
}

/// Stop the server a previous run of this app started, if it is still
/// running. Checks the process really is livekit-server first, so a reused
/// process id is never touched. Returns true if one was stopped.
async fn stop_orphan() -> bool {
    let Some(path) = pid_file() else { return false };
    let Some(pid) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
    else {
        return false;
    };
    let _ = std::fs::remove_file(&path);
    if !process_is_livekit_server(pid) {
        return false;
    }
    terminate(pid);
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !process_is_livekit_server(pid) {
            return true;
        }
    }
    true
}

#[cfg(unix)]
fn process_is_livekit_server(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("livekit-server"))
        .unwrap_or(false)
}

#[cfg(windows)]
fn process_is_livekit_server(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .to_ascii_lowercase()
                .contains("livekit-server")
        })
        .unwrap_or(false)
}

#[cfg(unix)]
fn terminate(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
}

#[cfg(windows)]
fn terminate(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .status();
}

async fn port_open(port: u16) -> bool {
    tokio::time::timeout(
        Duration::from_millis(300),
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

impl LocalServer {
    pub fn state(&self) -> ServerState {
        self.inner.lock().state.clone()
    }

    pub fn stats(&self) -> ServerStats {
        self.inner.lock().stats.clone()
    }

    pub fn is_usable(&self) -> bool {
        matches!(self.state(), ServerState::Running | ServerState::External)
    }

    /// Start the server, or adopt one already listening on the port.
    pub async fn start(&self, opts: LocalServerOptions) -> Result<()> {
        {
            let mut inner = self.inner.lock();
            if matches!(inner.state, ServerState::Running | ServerState::Starting) {
                return Ok(());
            }
            inner.state = ServerState::Starting;
            inner.stats = ServerStats::default();
            inner.prev_metrics = None;
        }
        // A server this app started in an earlier run that ended without
        // cleaning up (crash, force quit) is orphaned: its log pipe is gone,
        // so it dies on its next log line. Replace it rather than adopt it.
        if stop_orphan().await {
            log::info!("stopped a livekit-server left by an earlier run");
        }
        // A server left by the previous run may still be shutting down: it
        // keeps the port open for a moment, then refuses connections. Adopt
        // one only if it is still listening a second later.
        let adopt = port_open(opts.port).await && {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            port_open(opts.port).await
        };
        if !adopt && port_open(opts.port).await {
            // Still closing: give it a little longer before starting ours.
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(250)).await;
                if !port_open(opts.port).await {
                    break;
                }
            }
        }
        if adopt {
            let mut inner = self.inner.lock();
            inner.state = ServerState::External;
            inner.stats.started_at = None;
            return Ok(());
        }
        let result = self.spawn(&opts).await;
        if let Err(e) = &result {
            self.inner.lock().state = ServerState::Failed(format!("{e:#}"));
        }
        result
    }

    async fn spawn(&self, opts: &LocalServerOptions) -> Result<()> {
        let binary = find_binary(&opts.binary_path).ok_or_else(|| {
            anyhow!("livekit-server not found. Run the setup script or set its path in the Local dev profile.")
        })?;
        let dir = directories::ProjectDirs::from("io", "czoli1976", "livekit-workbench")
            .map(|d| d.data_dir().to_path_buf())
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&dir).ok();
        let cfg_path = dir.join("livekit-server.yaml");
        std::fs::write(&cfg_path, config_yaml(opts)).context("writing the server config")?;

        let bind = if opts.bind_all {
            "0.0.0.0"
        } else {
            "127.0.0.1"
        };
        let mut child = Command::new(&binary)
            .arg("--dev")
            .arg("--bind")
            .arg(bind)
            .arg("--config")
            .arg(&cfg_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting {}", binary.display()))?;
        if let (Some(pid), Some(path)) = (child.id(), pid_file()) {
            let _ = std::fs::write(path, pid.to_string());
        }

        for pipe in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let inner = self.inner.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(pipe).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    record_log_line(&inner, &line);
                }
            });
        }
        {
            let mut inner = self.inner.lock();
            inner.child = Some(child);
            inner.stats.started_at = Some(Instant::now());
        }

        // Ready when the signal port answers, or fail after 10 s.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if port_open(opts.port).await {
                self.inner.lock().state = ServerState::Running;
                return Ok(());
            }
            let exited = {
                let mut inner = self.inner.lock();
                inner
                    .child
                    .as_mut()
                    .and_then(|c| c.try_wait().ok().flatten())
            };
            if let Some(status) = exited {
                let last = self.last_error_line();
                return Err(anyhow!("livekit-server exited ({status}). {last}"));
            }
            if Instant::now() > deadline {
                self.stop().await;
                return Err(anyhow!(
                    "livekit-server did not start listening within 10 s"
                ));
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    fn last_error_line(&self) -> String {
        let inner = self.inner.lock();
        inner
            .stats
            .log
            .iter()
            .rev()
            .find(|l| l.level == "ERROR" || l.level == "FATAL")
            .map(|l| l.text.clone())
            .unwrap_or_default()
    }

    /// Stop the server if the app started it. An external server is left alone.
    pub async fn stop(&self) {
        let child = {
            let mut inner = self.inner.lock();
            inner.state = ServerState::Stopped;
            inner.child.take()
        };
        if let Some(mut child) = child {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            remove_pid_file();
        }
    }

    /// Kill synchronously; used when the app exits.
    pub fn kill_now(&self) {
        let mut inner = self.inner.lock();
        if let Some(child) = inner.child.as_mut() {
            let _ = child.start_kill();
            // Wait briefly so the next launch does not find it half closed.
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(2) {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if inner.child.take().is_some() {
            remove_pid_file();
        }
        inner.state = ServerState::Stopped;
    }

    /// Refresh stats from Prometheus and the room API. Call about once a second.
    pub async fn poll(&self, opts: &LocalServerOptions) {
        if !self.is_usable() {
            return;
        }
        // A server we adopted can go away (often one still shutting down from a
        // previous run): start our own instead of pointing at a closed port.
        if self.state() == ServerState::External && !port_open(opts.port).await {
            log::info!(
                "external server on port {} went away; starting one",
                opts.port
            );
            self.inner.lock().state = ServerState::Stopped;
            if let Err(e) = self.start(opts.clone()).await {
                log::warn!("local server: {e:#}");
            }
            return;
        }
        // The process may have died on its own.
        let died = {
            let mut inner = self.inner.lock();
            inner
                .child
                .as_mut()
                .and_then(|c| c.try_wait().ok().flatten())
        };
        if let Some(status) = died {
            let msg = format!(
                "livekit-server exited ({status}). {}",
                self.last_error_line()
            );
            let mut inner = self.inner.lock();
            inner.child = None;
            inner.state = ServerState::Failed(msg);
            return;
        }

        let owned = self.state() == ServerState::Running;
        if owned {
            if let Ok(text) = http_get_local(opts.port + PROMETHEUS_PORT_OFFSET, "/metrics").await {
                self.apply_metrics(&parse_prometheus(&text));
            }
        }
        let host = format!("http://127.0.0.1:{}", opts.port);
        let client = livekit_api::services::room::RoomClient::with_api_key(
            &host,
            LOCAL_DEV_KEY,
            LOCAL_DEV_SECRET,
        );
        if let Ok(rooms) = client.list_rooms(vec![]).await {
            let mut names = Vec::new();
            for room in &rooms {
                if let Ok(ps) = client.list_participants(&room.name).await {
                    names.extend(ps.into_iter().map(|p| p.identity));
                }
            }
            let mut inner = self.inner.lock();
            inner.stats.rooms = Some(rooms.len() as u64);
            inner.stats.participants = Some(names.len() as u64);
            inner.stats.room_names = rooms.into_iter().map(|r| r.name).collect();
            inner.stats.participant_names = names;
        }
    }

    fn apply_metrics(&self, m: &HashMap<String, f64>) {
        let now = Instant::now();
        let mut inner = self.inner.lock();
        inner.stats.memory_mb = m
            .get("process_resident_memory_bytes")
            .map(|b| b / 1_048_576.0);
        let inner = &mut *inner;
        if let Some((then, prev)) = &inner.prev_metrics {
            let dt = now.duration_since(*then).as_secs_f64().max(0.1);
            let rate = |key: &str| -> Option<f64> {
                let a = m.get(key)?;
                let b = prev.get(key)?;
                Some(((a - b) / dt).max(0.0))
            };
            inner.stats.cpu_percent = rate("process_cpu_seconds_total").map(|r| r * 100.0);
            inner.stats.packets_in_per_s = rate("livekit_packet_total|incoming");
            inner.stats.packets_out_per_s = rate("livekit_packet_total|outgoing");
            inner.stats.mbps_in = rate("livekit_packet_bytes|incoming").map(|b| b * 8.0 / 1e6);
            inner.stats.mbps_out = rate("livekit_packet_bytes|outgoing").map(|b| b * 8.0 / 1e6);
            inner.stats.nack_per_s = rate("livekit_nack_total");
        }
        let total = |k: &str| m.get(k).map(|v| *v as u64);
        inner.stats.fec_received = total("livekit_fec_total#received");
        inner.stats.fec_recovered = total("livekit_fec_total#recovered");
        inner.stats.fec_discarded = total("livekit_fec_total#discarded");
        inner.stats.nack_total = total("livekit_nack_total");
        inner.prev_metrics = Some((now, m.clone()));
    }
}

fn record_log_line(inner: &Arc<Mutex<Inner>>, line: &str) {
    // livekit-server logs "timestamp<TAB>LEVEL<TAB>logger<TAB>caller<TAB>message…"
    let mut parts = line.splitn(5, '\t');
    let _ts = parts.next();
    let level = parts.next().unwrap_or("").trim().to_string();
    let rest: Vec<&str> = parts.collect();
    let text = if rest.len() >= 3 {
        rest[2..].join(" ")
    } else {
        line.to_string()
    };
    let mut inner = inner.lock();
    // Only the server's own start-up line: participant logs carry client SDK versions.
    let startup = if line.contains("starting LiveKit server") {
        line
    } else {
        ""
    };
    if let Some(v) = startup
        .split("\"version\": \"")
        .nth(1)
        .and_then(|s| s.split('"').next())
    {
        inner.stats.version = v.to_string();
    }
    match level.as_str() {
        "WARN" => inner.stats.warnings += 1,
        "ERROR" | "FATAL" | "PANIC" => inner.stats.errors += 1,
        _ => {}
    }
    inner.stats.log.push_back(LogLine { level, text });
    while inner.stats.log.len() > LOG_LINES {
        inner.stats.log.pop_front();
    }
}

/// Minimal HTTP GET to a localhost port; enough for the Prometheus endpoint.
async fn http_get_local(port: u16, path: &str) -> Result<String> {
    let fut = async {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await?;
        let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes()).await?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await?;
        anyhow::Ok(String::from_utf8_lossy(&buf).into_owned())
    };
    let raw = tokio::time::timeout(Duration::from_secs(2), fut).await??;
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    // Chunked responses still parse: chunk-size lines are not valid metric lines.
    Ok(body)
}

/// Sum each metric over its series. Series with a `direction` label are also
/// summed under "name|direction", so in and out can be told apart.
pub fn parse_prometheus(text: &str) -> HashMap<String, f64> {
    let mut out: HashMap<String, f64> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name_labels, value) = match line.rsplit_once(' ') {
            Some(x) => x,
            None => continue,
        };
        let Ok(value) = value.parse::<f64>() else {
            continue;
        };
        let (name, labels) = match name_labels.split_once('{') {
            Some((n, l)) => (n, l.trim_end_matches('}')),
            None => (name_labels, ""),
        };
        *out.entry(name.to_string()).or_default() += value;
        for label in labels.split(',') {
            if let Some(dir) = label.trim().strip_prefix("direction=\"") {
                let dir = dir.trim_end_matches('"');
                *out.entry(format!("{name}|{dir}")).or_default() += value;
            }
            if let Some(kind) = label.trim().strip_prefix("type=\"") {
                let kind = kind.trim_end_matches('"');
                *out.entry(format!("{name}#{kind}")).or_default() += value;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prometheus_text() {
        let text = "# HELP x\n# TYPE x counter\nprocess_cpu_seconds_total 1.5\n\
                    livekit_packet_total{direction=\"incoming\",type=\"media\"} 10\n\
                    livekit_packet_total{direction=\"incoming\",type=\"rtcp\"} 5\n\
                    livekit_packet_total{direction=\"outgoing\",type=\"media\"} 7\n\
                    livekit_room_total 1\n";
        let m = parse_prometheus(text);
        assert_eq!(m["process_cpu_seconds_total"], 1.5);
        assert_eq!(m["livekit_packet_total"], 22.0);
        assert_eq!(m["livekit_packet_total|incoming"], 15.0);
        assert_eq!(m["livekit_packet_total|outgoing"], 7.0);
        assert_eq!(m["livekit_room_total"], 1.0);
    }

    #[test]
    fn prometheus_type_labels() {
        let m = parse_prometheus(
            "livekit_fec_total{direction=\"incoming\",node_id=\"x\",type=\"recovered\"} 7\nlivekit_fec_total{direction=\"incoming\",type=\"received\"} 40\n",
        );
        assert_eq!(m["livekit_fec_total#recovered"], 7.0);
        assert_eq!(m["livekit_fec_total#received"], 40.0);
        assert_eq!(m["livekit_fec_total|incoming"], 47.0);
    }

    #[test]
    fn config_has_prometheus_and_fec() {
        let o = LocalServerOptions {
            flexfec_upstream: true,
            ..Default::default()
        };
        let y = config_yaml(&o);
        assert!(y.contains("port: 7880"));
        assert!(y.contains("port: 8880"), "prometheus port");
        assert!(y.contains("upstream_enabled: true"));
        assert!(y.contains("devkey: secret"));
    }

    #[test]
    fn log_lines_are_classified() {
        let inner = Arc::new(Mutex::new(Inner::default()));
        record_log_line(
            &inner,
            "2026-11-16T13:55:53.643Z\tWARN\tlivekit\tx.go:1\tsomething odd",
        );
        record_log_line(
            &inner,
            "2026-11-16T13:55:53.643Z\tINFO\tlivekit\tserver.go:309\tstarting LiveKit server\t{\"version\": \"1.13.11\"}",
        );
        let s = inner.lock().stats.clone();
        assert_eq!(s.warnings, 1);
        assert_eq!(s.version, "1.13.11");
        assert_eq!(s.log[0].text, "something odd");
    }
}
