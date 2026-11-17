//! Network impairment without root or system changes: an in-app TURN relay.
//!
//! With impairment on, both of the app's connections are forced to relay
//! through a TURN server that runs inside the app on 127.0.0.1. The relay
//! has two listeners:
//!
//! * the publisher's: packets the relay forwards to the SFU (the publisher's
//!   uplink) go through the uplink shaper;
//! * the subscriber's: packets the relay hands back to the subscriber (the
//!   subscriber's downlink) go through the downlink shaper.
//!
//! Each shaper drops a share of packets at random, limits the rate with a
//! bottleneck queue (tail drop once it holds more than `MAX_QUEUE`), then adds
//! a fixed delay plus random jitter. Packet order is kept. Settings change
//! live. Feedback in the other direction (RTCP, NACKs) is not impaired.

use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::Instant;
use turn::auth::{generate_auth_key, AuthHandler};
use turn::relay::RelayAddressGenerator;
use turn::server::config::{ConnConfig, ServerConfig};
use turn::server::Server;
use webrtc_util::Conn;

/// The longest a packet waits in the bottleneck queue before it is dropped.
pub const MAX_QUEUE: Duration = Duration::from_millis(300);
const REALM: &str = "workbench";
const USER: &str = "workbench";

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Impairment {
    /// Random packet loss, percent.
    pub loss_pct: f32,
    /// Added one-way delay.
    pub delay_ms: u32,
    /// Extra random delay, 0..jitter_ms, without reordering.
    pub jitter_ms: u32,
    /// Bottleneck rate in kbps; 0 = unlimited.
    pub rate_kbps: u32,
}

impl Impairment {
    pub fn is_clear(&self) -> bool {
        self.loss_pct <= 0.0 && self.delay_ms == 0 && self.jitter_ms == 0 && self.rate_kbps == 0
    }
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct LinkCounters {
    pub passed: u64,
    pub bytes: u64,
    pub lost: u64,
    pub queue_dropped: u64,
    /// Bottleneck backlog when the last packet arrived.
    pub queue_ms: f64,
}

/// One direction's settings and counters, shared by the UI and the relay.
#[derive(Clone, Default)]
pub struct Link {
    pub settings: Arc<Mutex<Impairment>>,
    pub counters: Arc<Mutex<LinkCounters>>,
    /// Impair only once the connection is up. Delay during WebRTC m150's
    /// connection setup (ICE plus the DTLS handshake carried in STUN) made
    /// the publisher sometimes never connect.
    pub armed: Arc<std::sync::atomic::AtomicBool>,
}

/// The publisher's uplink and the subscriber's downlink.
#[derive(Clone, Default)]
pub struct Links {
    pub up: Link,
    pub down: Link,
}

/// Small xorshift generator: random enough for loss and jitter.
struct Rng(u64);
impl Rng {
    fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15);
        Self(seed | 1)
    }
    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Decides when (or whether) each packet leaves. Pure, so it can be tested.
struct Schedule {
    link_free: Instant,
    last_departure: Instant,
    rng: Rng,
}

enum Verdict {
    Lost,
    QueueFull,
    Send(Instant, f64),
}

impl Schedule {
    fn new(now: Instant) -> Self {
        Self {
            link_free: now,
            last_departure: now,
            rng: Rng::new(),
        }
    }

    fn admit(&mut self, s: &Impairment, len: usize, now: Instant) -> Verdict {
        if s.loss_pct > 0.0 && self.rng.unit() * 100.0 < s.loss_pct as f64 {
            return Verdict::Lost;
        }
        let mut t = now;
        let mut backlog = Duration::ZERO;
        if s.rate_kbps > 0 {
            let start = self.link_free.max(now);
            backlog = start - now;
            if backlog > MAX_QUEUE {
                return Verdict::QueueFull;
            }
            let tx = Duration::from_secs_f64(len as f64 * 8.0 / (s.rate_kbps as f64 * 1000.0));
            self.link_free = start + tx;
            t = self.link_free;
        }
        t += Duration::from_millis(s.delay_ms as u64);
        if s.jitter_ms > 0 {
            t += Duration::from_secs_f64(self.rng.unit() * s.jitter_ms as f64 / 1000.0);
        }
        // Never reorder: a packet leaves no earlier than the one before it.
        t = t.max(self.last_departure);
        self.last_departure = t;
        Verdict::Send(t, backlog.as_secs_f64() * 1000.0)
    }
}

/// A UDP socket whose `send_to` goes through a shaper.
struct ShapedConn {
    inner: Arc<UdpSocket>,
    tx: mpsc::UnboundedSender<(Vec<u8>, SocketAddr)>,
}

impl ShapedConn {
    fn new(inner: Arc<UdpSocket>, link: Link) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(shaper(inner.clone(), link, rx));
        Self { inner, tx }
    }
}

async fn shaper(
    sock: Arc<UdpSocket>,
    link: Link,
    mut rx: mpsc::UnboundedReceiver<(Vec<u8>, SocketAddr)>,
) {
    let mut sched = Schedule::new(Instant::now());
    let mut queue: VecDeque<(Instant, Vec<u8>, SocketAddr)> = VecDeque::new();
    loop {
        let next = queue.front().map(|p| p.0);
        tokio::select! {
            msg = rx.recv() => {
                let Some((data, addr)) = msg else { break };
                let s = if link.armed.load(std::sync::atomic::Ordering::Relaxed) {
                    *link.settings.lock()
                } else {
                    Impairment::default()
                };
                let now = Instant::now();
                if s.is_clear() && queue.is_empty() {
                    let _ = sock.send_to(&data, addr).await;
                    let mut c = link.counters.lock();
                    c.passed += 1;
                    c.bytes += data.len() as u64;
                    c.queue_ms = 0.0;
                    continue;
                }
                match sched.admit(&s, data.len(), now) {
                    Verdict::Lost => link.counters.lock().lost += 1,
                    Verdict::QueueFull => link.counters.lock().queue_dropped += 1,
                    Verdict::Send(at, backlog) => {
                        link.counters.lock().queue_ms = backlog;
                        queue.push_back((at, data, addr));
                    }
                }
            }
            _ = tokio::time::sleep_until(next.unwrap_or_else(Instant::now)), if next.is_some() => {
                let now = Instant::now();
                while queue.front().is_some_and(|p| p.0 <= now) {
                    let (_, data, addr) = queue.pop_front().expect("front");
                    let _ = sock.send_to(&data, addr).await;
                    let mut c = link.counters.lock();
                    c.passed += 1;
                    c.bytes += data.len() as u64;
                }
            }
        }
    }
}

#[async_trait]
impl Conn for ShapedConn {
    async fn connect(&self, addr: SocketAddr) -> webrtc_util::Result<()> {
        Ok(self.inner.connect(addr).await?)
    }
    async fn recv(&self, buf: &mut [u8]) -> webrtc_util::Result<usize> {
        Ok(self.inner.recv(buf).await?)
    }
    async fn recv_from(&self, buf: &mut [u8]) -> webrtc_util::Result<(usize, SocketAddr)> {
        Ok(self.inner.recv_from(buf).await?)
    }
    async fn send(&self, buf: &[u8]) -> webrtc_util::Result<usize> {
        Ok(self.inner.send(buf).await?)
    }
    async fn send_to(&self, buf: &[u8], target: SocketAddr) -> webrtc_util::Result<usize> {
        let _ = self.tx.send((buf.to_vec(), target));
        Ok(buf.len())
    }
    fn local_addr(&self) -> webrtc_util::Result<SocketAddr> {
        Ok(self.inner.local_addr()?)
    }
    fn remote_addr(&self) -> Option<SocketAddr> {
        self.inner.peer_addr().ok()
    }
    async fn close(&self) -> webrtc_util::Result<()> {
        Ok(())
    }
    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        self
    }
}

/// Relay sockets toward the SFU; optionally shaped (the uplink).
struct Relays {
    /// The address the SFU sees our relay at.
    advertise: IpAddr,
    shape: Option<Link>,
}

#[async_trait]
impl RelayAddressGenerator for Relays {
    fn validate(&self) -> Result<(), turn::Error> {
        Ok(())
    }
    fn supports_family(&self, use_ipv4: bool) -> bool {
        use_ipv4
    }
    async fn allocate_conn(
        &self,
        _use_ipv4: bool,
        requested_port: u16,
    ) -> Result<(Arc<dyn Conn + Send + Sync>, SocketAddr), turn::Error> {
        let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, requested_port)).await?;
        let mut addr = sock.local_addr()?;
        addr.set_ip(self.advertise);
        let sock = Arc::new(sock);
        let conn: Arc<dyn Conn + Send + Sync> = match &self.shape {
            Some(link) => Arc::new(ShapedConn::new(sock, link.clone())),
            None => sock,
        };
        Ok((conn, addr))
    }
}

struct Auth {
    password: String,
}

impl AuthHandler for Auth {
    fn auth_handle(
        &self,
        username: &str,
        realm: &str,
        _src: SocketAddr,
    ) -> Result<Vec<u8>, turn::Error> {
        if username != USER {
            return Err(turn::Error::ErrNoSuchUser);
        }
        Ok(generate_auth_key(username, realm, &self.password))
    }
}

/// The running relay. Dropping it does not stop it: call `close`.
pub struct Relay {
    server: Server,
    pub pub_port: u16,
    pub sub_port: u16,
    password: String,
}

impl Relay {
    /// Start both listeners on 127.0.0.1. `sfu_url` decides which local
    /// address the relay sockets advertise.
    pub async fn start(sfu_url: &str, links: &Links) -> Result<Self> {
        // The SFU answers a relay address from the matching local interface, and
        // TURN permissions are per address: advertise the LAN address (a local
        // server's candidates use it too), falling back to the route to the server.
        let advertise = match local_ip_toward("udp://192.0.2.1:9").await {
            Ok(ip) => ip,
            Err(_) => local_ip_toward(sfu_url)
                .await
                .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        };
        let password = format!("{:016x}", Rng::new().0);

        let pub_sock = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?);
        let sub_sock = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?);
        let pub_port = pub_sock.local_addr()?.port();
        let sub_port = sub_sock.local_addr()?.port();

        let server = Server::new(ServerConfig {
            conn_configs: vec![
                ConnConfig {
                    conn: pub_sock,
                    relay_addr_generator: Box::new(Relays {
                        advertise,
                        shape: Some(links.up.clone()),
                    }),
                },
                ConnConfig {
                    // What the relay hands back to the subscriber leaves
                    // through this socket: shape it for the downlink.
                    conn: Arc::new(ShapedConn::new(sub_sock, links.down.clone())),
                    relay_addr_generator: Box::new(Relays {
                        advertise,
                        shape: None,
                    }),
                },
            ],
            realm: REALM.into(),
            auth_handler: Arc::new(Auth {
                password: password.clone(),
            }),
            channel_bind_timeout: Duration::from_secs(0),
            alloc_close_notify: None,
        })
        .await
        .map_err(|e| anyhow!("starting the impairment relay: {e}"))?;
        log::info!("impairment relay on 127.0.0.1:{pub_port} (publisher) and :{sub_port} (subscriber), advertising {advertise}");
        Ok(Self {
            server,
            pub_port,
            sub_port,
            password,
        })
    }

    /// RTC settings that force a connection through this relay.
    pub fn rtc_config(
        &self,
        publisher: bool,
    ) -> livekit::webrtc::peer_connection_factory::RtcConfiguration {
        use livekit::webrtc::peer_connection_factory::{
            IceServer, IceTransportsType, RtcConfiguration,
        };
        let port = if publisher {
            self.pub_port
        } else {
            self.sub_port
        };
        let mut c = RtcConfiguration::default();
        c.ice_servers = vec![IceServer {
            urls: vec![format!("turn:127.0.0.1:{port}?transport=udp")],
            username: USER.into(),
            password: self.password.clone(),
        }];
        c.ice_transport_type = IceTransportsType::Relay;
        c
    }

    pub async fn close(self) {
        let _ = self.server.close().await;
    }
}

/// The local address this machine uses to reach the server in `url`.
async fn local_ip_toward(url: &str) -> Result<IpAddr> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let hostport = rest.split('/').next().unwrap_or(rest);
    let hostport = if hostport.contains(':') && !hostport.starts_with('[') {
        hostport.to_string()
    } else {
        format!("{hostport}:443")
    };
    let target = tokio::net::lookup_host(&hostport)
        .await
        .context("resolving the server")?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| anyhow!("no IPv4 address for the server"))?;
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    probe.connect(target).await?;
    Ok(probe.local_addr()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_spaces_packets_and_tail_drops() {
        let now = Instant::now();
        let mut s = Schedule::new(now);
        // 1000 kbps: a 1250-byte packet takes 10 ms on the wire.
        let imp = Impairment {
            rate_kbps: 1000,
            ..Default::default()
        };
        let mut sent = Vec::new();
        let mut full = 0;
        for _ in 0..40 {
            match s.admit(&imp, 1250, now) {
                Verdict::Send(t, _) => sent.push(t),
                Verdict::QueueFull => full += 1,
                Verdict::Lost => unreachable!(),
            }
        }
        assert_eq!(sent[0] - now, Duration::from_millis(10));
        assert_eq!(sent[1] - sent[0], Duration::from_millis(10));
        // 300 ms of queue holds about 30 packets plus the one on the wire.
        assert!((30..=32).contains(&sent.len()), "{}", sent.len());
        assert_eq!(full, 40 - sent.len());
    }

    #[test]
    fn delay_and_jitter_keep_order() {
        let now = Instant::now();
        let mut s = Schedule::new(now);
        let imp = Impairment {
            delay_ms: 50,
            jitter_ms: 30,
            ..Default::default()
        };
        let mut last = now;
        for _ in 0..200 {
            let Verdict::Send(t, _) = s.admit(&imp, 100, now) else {
                panic!()
            };
            assert!(t >= now + Duration::from_millis(50));
            assert!(t < now + Duration::from_millis(81));
            assert!(t >= last);
            last = t;
        }
    }

    #[test]
    fn loss_is_roughly_the_setting() {
        let now = Instant::now();
        let mut s = Schedule::new(now);
        let imp = Impairment {
            loss_pct: 10.0,
            ..Default::default()
        };
        let lost = (0..20_000)
            .filter(|_| matches!(s.admit(&imp, 100, now), Verdict::Lost))
            .count();
        assert!((1600..2400).contains(&lost), "{lost}");
    }
}

#[cfg(test)]
mod shaped_conn_tests {
    use super::*;

    /// Packets sent through a delayed link all arrive, in order, after the delay.
    #[tokio::test]
    async fn delayed_packets_arrive_in_order() {
        let link = Link::default();
        *link.settings.lock() = Impairment {
            delay_ms: 40,
            ..Default::default()
        };
        link.armed.store(true, std::sync::atomic::Ordering::Relaxed);
        let sink = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let to = sink.local_addr().unwrap();
        let src = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap());
        let conn = ShapedConn::new(src, link.clone());
        let t0 = Instant::now();
        for i in 0..20u8 {
            conn.send_to(&[i], to).await.unwrap();
        }
        let mut buf = [0u8; 4];
        for i in 0..20u8 {
            let (n, _) = tokio::time::timeout(Duration::from_secs(2), sink.recv_from(&mut buf))
                .await
                .expect("packet arrives")
                .unwrap();
            assert_eq!((n, buf[0]), (1, i));
        }
        assert!(t0.elapsed() >= Duration::from_millis(40));
        assert_eq!(link.counters.lock().passed, 20);
    }
}
