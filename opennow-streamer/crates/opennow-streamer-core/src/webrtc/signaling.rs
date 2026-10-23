use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use opennow_streamer_protocol::Session;
use opennow_streamer_transport::webrtc::IceCandidate;
use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::{HeaderValue, Uri};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use super::{Failure, MAX_SDP_BYTES};

pub(super) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_SIGNALING_BYTES: usize = 512 * 1024;

struct ResolveRequest {
    host: String,
    port: u16,
    result: mpsc::SyncSender<std::io::Result<Vec<SocketAddr>>>,
}

pub(super) fn resolve(
    host: String,
    port: u16,
    cancelled: &AtomicBool,
) -> Result<Vec<SocketAddr>, Failure> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    static RESOLVER: OnceLock<Option<mpsc::SyncSender<ResolveRequest>>> = OnceLock::new();
    let resolver = RESOLVER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel::<ResolveRequest>(1);
            thread::Builder::new()
                .name("opennow-webrtc-dns".to_owned())
                .spawn(move || {
                    while let Ok(request) = receiver.recv() {
                        let result = (request.host.as_str(), request.port)
                            .to_socket_addrs()
                            .map(|addresses| addresses.take(8).collect::<Vec<_>>());
                        let _ = request.result.try_send(result);
                    }
                })
                .ok()
                .map(|_| sender)
        })
        .as_ref()
        .ok_or_else(|| Failure::signaling("Could not start bounded DNS resolver"))?;
    let (sender, receiver) = mpsc::sync_channel(1);
    resolver
        .try_send(ResolveRequest {
            host,
            port,
            result: sender,
        })
        .map_err(|_| Failure::signaling("WebRTC DNS resolver is busy"))?;
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(Failure::signaling("WebRTC DNS cancelled or timed out"));
        }
        match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(Ok(addresses)) if !addresses.is_empty() => return Ok(addresses),
            Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Failure::signaling("WebRTC DNS failed"));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

pub(super) enum Incoming {
    Offer(String),
    Candidate(IceCandidate),
    Closed(String),
}

/// Names the transport-level reason without addresses, URLs or server text.
fn error_detail(error: &tungstenite::Error) -> String {
    match error {
        tungstenite::Error::ConnectionClosed => "connection-closed".to_owned(),
        tungstenite::Error::AlreadyClosed => "already-closed".to_owned(),
        tungstenite::Error::Io(error) => match error.raw_os_error() {
            Some(code) => format!("io kind={:?} os_code={code}", error.kind()),
            None => format!("io kind={:?} os_code=none", error.kind()),
        },
        tungstenite::Error::Tls(_) => "tls".to_owned(),
        tungstenite::Error::Capacity(_) => "capacity".to_owned(),
        tungstenite::Error::Protocol(error) => {
            let variant = format!("{error:?}");
            format!(
                "protocol={}",
                variant.split(['(', ' ', '{']).next().unwrap_or_default()
            )
        }
        tungstenite::Error::WriteBufferFull(_) => "write-buffer-full".to_owned(),
        tungstenite::Error::Utf8(_) => "utf8".to_owned(),
        tungstenite::Error::AttackAttempt => "attack-attempt".to_owned(),
        tungstenite::Error::Url(_) => "url".to_owned(),
        tungstenite::Error::Http(response) => format!("http status={}", response.status().as_u16()),
        tungstenite::Error::HttpFormat(_) => "http-format".to_owned(),
    }
}

fn failure(context: &str, error: &tungstenite::Error) -> Failure {
    let detail = error_detail(error);
    opennow_streamer_protocol::log::log_line(
        "WARN",
        "webrtc-signaling",
        &format!("{context} {detail}"),
    );
    Failure::signaling(format!("{context} ({detail})"))
}

pub(super) struct Protocol {
    peer_name: String,
    peer_id: u64,
    remote_id: u64,
    ack: u64,
    noted_unhandled: u8,
    pub(super) received: u8,
}

impl Protocol {
    pub(super) fn new(peer_name: String) -> Self {
        Self {
            peer_name,
            peer_id: 0,
            remote_id: 1,
            ack: 0,
            noted_unhandled: 0,
            received: 0,
        }
    }

    pub(super) fn peer_info(&mut self, width: u32, height: u32) -> Value {
        self.ack += 1;
        json!({"ackid":self.ack,"peer_info":{
            "browser":"Chrome","browserVersion":"131","connected":true,
            "id":self.peer_id,"name":self.peer_name,"peerRole":1,
            "resolution":format!("{width}x{height}"),"version":2
        }})
    }

    pub(super) fn peer_message(&mut self, payload: Value) -> Value {
        self.ack += 1;
        json!({"ackid":self.ack,"peer_msg":{
            "from":self.peer_id,"to":self.remote_id,"msg":payload.to_string()
        }})
    }

    pub(super) fn bye(&mut self) -> Value {
        self.ack += 1;
        json!({"ackid":self.ack,"peer_msg":{
            "from":self.peer_id,"to":self.remote_id,"msg":"BYE"
        }})
    }

    pub(super) fn receive(
        &mut self,
        text: &str,
    ) -> Result<(Vec<Value>, Option<Incoming>), Failure> {
        if text.len() > MAX_SIGNALING_BYTES {
            return Err(Failure::signaling("Signaling message exceeds size limit"));
        }
        let packet: Value =
            serde_json::from_str(text).map_err(|_| Failure::signaling("Invalid signaling JSON"))?;
        self.received = self.received.saturating_add(1);
        if self.received <= 6 {
            let keys = packet
                .as_object()
                .map(|object| {
                    object
                        .keys()
                        .take(8)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_else(|| "non-object".to_owned());
            opennow_streamer_protocol::log::log_line(
                "WARN",
                "webrtc-signaling",
                &format!("rx {} keys={keys}", self.received),
            );
        }
        if packet["peer_info"]["name"].as_str() == Some(&self.peer_name)
            && let Some(id) = packet["peer_info"]["id"].as_u64()
        {
            self.peer_id = id;
        }
        let mut replies = Vec::new();
        // The seat sends the offer only after the client acks the peer_info
        // assignment. Skipping that ack, because the id now matches, leaves
        // the socket open and silent.
        if let Some(ack) = packet["ackid"].as_u64()
            && packet["peer_msg"]["from"].as_u64() != Some(self.peer_id)
        {
            replies.push(json!({"ack":ack}));
        }
        if packet["hb"].as_u64().is_some_and(|hb| hb != 0) {
            replies.push(json!({"hb":1}));
            return Ok((replies, None));
        }
        if packet["error"].as_str() == Some("peerRemoved") {
            return Ok((replies, Some(Incoming::Closed("peer-removed".to_owned()))));
        }
        if packet.get("error").is_some_and(|value| !value.is_null()) {
            return Err(Failure::signaling("Server rejected signaling request"));
        }
        if let Some(incoming) = self.incoming_offer(&packet)? {
            return Ok((replies, Some(incoming)));
        }
        let Some(message) = packet.get("peer_msg").and_then(|peer| peer.get("msg")) else {
            if packet.get("peer_info").is_none() && packet.get("ack").is_none() {
                self.note_unhandled(&packet);
            }
            return Ok((replies, None));
        };
        if let Some(id) = packet["peer_msg"]["from"].as_u64() {
            self.remote_id = id;
        }
        if message.as_str().is_some_and(|text| text.trim() == "BYE") {
            return Ok((replies, Some(Incoming::Closed("bye".to_owned()))));
        }
        let payload = peer_payload(message)?;
        if payload["type"].as_str() == Some("offer") {
            let sdp = payload["sdp"]
                .as_str()
                .filter(|sdp| !sdp.is_empty() && sdp.len() <= MAX_SDP_BYTES)
                .ok_or_else(|| Failure::signaling("Offer SDP is missing or exceeds size limit"))?;
            return Ok((replies, Some(Incoming::Offer(sdp.to_owned()))));
        }
        if payload["candidate"].is_string() {
            let mut candidate: IceCandidate = serde_json::from_value(payload)
                .map_err(|_| Failure::signaling("Invalid remote ICE candidate"))?;
            if candidate.candidate.len() > 4096 {
                return Err(Failure::signaling(
                    "Remote ICE candidate exceeds size limit",
                ));
            }
            if candidate.sdp_mid.is_none() && candidate.sdp_m_line_index.is_none() {
                candidate.sdp_m_line_index = Some(0);
            }
            return Ok((replies, Some(Incoming::Candidate(candidate))));
        }
        self.note_unhandled(&payload);
        Ok((replies, None))
    }

    fn incoming_offer(&mut self, packet: &Value) -> Result<Option<Incoming>, Failure> {
        if packet["type"].as_str() != Some("offer") {
            return Ok(None);
        }
        let sdp = packet["sdp"]
            .as_str()
            .filter(|sdp| !sdp.is_empty() && sdp.len() <= MAX_SDP_BYTES)
            .ok_or_else(|| Failure::signaling("Offer SDP is missing or exceeds size limit"))?;
        Ok(Some(Incoming::Offer(sdp.to_owned())))
    }

    fn note_unhandled(&mut self, value: &Value) {
        self.noted_unhandled = self.noted_unhandled.saturating_add(1);
        if self.noted_unhandled > 4 {
            return;
        }
        let keys = value
            .as_object()
            .map(|object| {
                object
                    .keys()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_else(|| "non-object".to_owned());
        opennow_streamer_protocol::log::log_line(
            "WARN",
            "webrtc-signaling",
            &format!("unhandled signaling message keys={keys}"),
        );
    }
}

fn peer_payload(message: &Value) -> Result<Value, Failure> {
    match message {
        Value::Object(_) => Ok(message.clone()),
        Value::String(text) if text.trim_start().starts_with("v=") => {
            Ok(json!({"type":"offer","sdp":text}))
        }
        Value::String(text) => serde_json::from_str(text)
            .map_err(|_| Failure::signaling("Invalid nested peer message")),
        _ => Err(Failure::signaling("Invalid nested peer message")),
    }
}

fn query_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

pub(super) fn sign_in_url(session: &Session, peer: &str) -> Result<String, Failure> {
    let base = session
        .extra
        .get("signalingUrl")
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| Failure::signaling("WebRTC requires an explicit signalingUrl"))?;
    let uri: Uri = base
        .parse()
        .map_err(|_| Failure::signaling("Invalid signalingUrl"))?;
    let authority = uri
        .authority()
        .ok_or_else(|| Failure::signaling("Signaling URL has no host"))?;
    let test_plaintext =
        cfg!(test) && uri.scheme_str() == Some("ws") && uri.host() == Some("127.0.0.1");
    if (uri.scheme_str() != Some("wss") && !test_plaintext)
        || authority.as_str().contains('@')
        || base.contains('#')
    {
        return Err(Failure::signaling(
            "Signaling requires a credential-free WSS endpoint",
        ));
    }
    let path = uri.path().trim_end_matches('/');
    let path = if path.ends_with("/sign_in") {
        path.to_owned()
    } else {
        format!("{path}/sign_in")
    };
    // Desktop signs a new seat in at version 2. version=3.0 with reconnect=1 is
    // only for a peer the seat already accepted, and a new session does not
    // set that flag.
    let reconnect = session
        .extra
        .get("signalingReconnect")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let scheme = if test_plaintext { "ws" } else { "wss" };
    Ok(format!(
        "{scheme}://{authority}{path}?peer_id={}&version={}&peer_role=1&pairing_id={}{}",
        query_component(peer),
        if reconnect { "3.0" } else { "2" },
        query_component(&session.session_id),
        if reconnect { "&reconnect=1" } else { "" }
    ))
}

pub(super) struct Signaling<Stream: Read + Write = MaybeTlsStream<TcpStream>> {
    socket: WebSocket<Stream>,
    pub(super) protocol: Protocol,
    heartbeat_at: Instant,
    write_pending_since: Option<Instant>,
    /// Cleared only for the same-peer offer retry. Every other close tells the
    /// server this peer is gone so the next launch can sign in.
    release_peer: bool,
}

fn generate_peer() -> Result<String, Failure> {
    let mut random = [0_u8; 12];
    getrandom::fill(&mut random)
        .map_err(|_| Failure::signaling("Could not generate signaling identity"))?;
    Ok(format!(
        "peer-{}",
        random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

impl Signaling {
    pub(super) fn connect(session: &Session, cancelled: &AtomicBool) -> Result<Self, Failure> {
        let peer = generate_peer()?;
        let url = sign_in_url(session, &peer)?;
        opennow_streamer_protocol::log::log_line(
            "WARN",
            "webrtc-signaling",
            &format!(
                "sign-in version={} reconnect={}",
                if url.contains("version=3.0") {
                    "3.0"
                } else {
                    "2"
                },
                if url.contains("reconnect=1") { "1" } else { "0" }
            ),
        );
        let mut request = url
            .into_client_request()
            .map_err(|_| Failure::signaling("Invalid WSS request"))?;
        request.headers_mut().insert(
            "Origin",
            HeaderValue::from_static("https://play.geforcenow.com"),
        );
        request.headers_mut().insert(
            "User-Agent",
            HeaderValue::from_static(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/131.0.0.0 Safari/537.36",
            ),
        );
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_str(&format!("x-nv-sessionid.{}", session.session_id))
                .map_err(|_| Failure::signaling("Invalid signaling session ID"))?,
        );
        let host = request
            .uri()
            .host()
            .ok_or_else(|| Failure::signaling("Missing signaling host"))?
            .to_owned();
        let port = request.uri().port_u16().unwrap_or(443);
        let addresses = resolve(host, port, cancelled)?;
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let mut connected = None;
        for address in addresses {
            if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                break;
            }
            let budget = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(500));
            if let Ok(stream) = TcpStream::connect_timeout(&address, budget) {
                connected = Some(stream);
                break;
            }
        }
        let stream = connected
            .ok_or_else(|| Failure::signaling("Signaling TCP connection failed or timed out"))?;
        stream
            .set_nonblocking(true)
            .map_err(|_| Failure::signaling("Could not bound signaling handshake"))?;
        let config = WebSocketConfig::default()
            .max_write_buffer_size(MAX_SIGNALING_BYTES * 2)
            .max_message_size(Some(MAX_SIGNALING_BYTES))
            .max_frame_size(Some(MAX_SIGNALING_BYTES));
        let mut handshake =
            tungstenite::client_tls_with_config(request, stream, Some(config), None);
        let mut socket = loop {
            if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Err(Failure::signaling(
                    "Signaling handshake cancelled or timed out",
                ));
            }
            match handshake {
                Ok((socket, _)) => break socket,
                Err(tungstenite::HandshakeError::Interrupted(pending)) => {
                    thread::sleep(Duration::from_millis(5));
                    handshake = pending.handshake();
                }
                Err(tungstenite::HandshakeError::Failure(error)) => {
                    return Err(failure("Signaling TLS/WebSocket handshake failed", &error));
                }
            }
        };
        let stream = match socket.get_mut() {
            MaybeTlsStream::Plain(stream) => stream,
            MaybeTlsStream::Rustls(stream) => stream.get_mut(),
            _ => return Err(Failure::signaling("Unsupported signaling TLS backend")),
        };
        stream
            .set_nonblocking(true)
            .map_err(|_| Failure::signaling("Could not configure signaling socket"))?;
        Ok(Self {
            socket,
            protocol: Protocol::new(peer),
            heartbeat_at: Instant::now(),
            write_pending_since: None,
            release_peer: true,
        })
    }
}

impl<Stream: Read + Write> Drop for Signaling<Stream> {
    fn drop(&mut self) {
        if self.release_peer {
            let _ = self.send_bye();
        }
    }
}

impl<Stream: Read + Write> Signaling<Stream> {
    fn write_result(&mut self, result: Result<(), tungstenite::Error>) -> Result<(), Failure> {
        match result {
            Ok(()) => {
                self.write_pending_since = None;
                Ok(())
            }
            Err(tungstenite::Error::Io(error)) if error.kind() == ErrorKind::WouldBlock => {
                let since = self.write_pending_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= WRITE_TIMEOUT {
                    return Err(Failure::signaling("Signaling pending write timed out"));
                }
                Ok(())
            }
            Err(tungstenite::Error::WriteBufferFull(_)) => Err(Failure::signaling(
                "Signaling write buffer exceeds size limit",
            )),
            Err(error) => Err(failure("Signaling write failed", &error)),
        }
    }

    fn flush(&mut self) -> Result<(), Failure> {
        if self
            .write_pending_since
            .is_some_and(|since| since.elapsed() >= WRITE_TIMEOUT)
        {
            return Err(Failure::signaling("Signaling pending write timed out"));
        }
        let result = self.socket.flush();
        self.write_result(result)
    }

    pub(super) fn send(&mut self, payload: Value) -> Result<(), Failure> {
        let text = payload.to_string();
        if text.len() > MAX_SIGNALING_BYTES {
            return Err(Failure::signaling(
                "Outgoing signaling message exceeds size limit",
            ));
        }
        let result = self.socket.send(Message::Text(text.into()));
        self.write_result(result)
    }

    pub(super) fn send_peer(&mut self, payload: Value) -> Result<(), Failure> {
        let packet = self.protocol.peer_message(payload);
        self.send(packet)
    }

    pub(super) fn send_bye(&mut self) -> Result<(), Failure> {
        let packet = self.protocol.bye();
        self.send(packet)?;
        let deadline = Instant::now() + WRITE_TIMEOUT;
        loop {
            self.flush()?;
            if self.write_pending_since.is_none() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Failure::signaling("Signaling pending write timed out"));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    pub(super) fn poll(&mut self) -> Result<Option<Incoming>, Failure> {
        self.flush()?;
        if self.heartbeat_at.elapsed() >= HEARTBEAT_INTERVAL {
            self.send(json!({"hb":1}))?;
            self.heartbeat_at = Instant::now();
        }
        let packet = match self.socket.read() {
            Ok(Message::Text(text)) => text.to_string(),
            Ok(Message::Binary(bytes)) => String::from_utf8(bytes.to_vec())
                .map_err(|_| Failure::signaling("Invalid signaling UTF-8"))?,
            Ok(Message::Close(frame)) => {
                let detail = frame.map_or_else(
                    || "close_code=none".to_owned(),
                    |frame| format!("close_code={}", u16::from(frame.code)),
                );
                opennow_streamer_protocol::log::log_line(
                    "WARN",
                    "webrtc-signaling",
                    &format!("peer-close {detail}"),
                );
                return Ok(Some(Incoming::Closed(detail)));
            }
            Ok(_) => return Ok(None),
            Err(tungstenite::Error::Io(error))
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(failure("Signaling connection closed", &error)),
        };
        let (replies, incoming) = self.protocol.receive(&packet)?;
        for reply in replies {
            self.send(reply)?;
        }
        Ok(incoming)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opennow_streamer_protocol::Session;

    fn session(extra: serde_json::Value) -> Session {
        let mut value = json!({
            "sessionId": "seat-1",
            "serverIp": "203.0.113.5",
            "signalingUrl": "wss://203.0.113.5/sign_in"
        });
        let target = value.as_object_mut().expect("session object");
        for (key, item) in extra.as_object().into_iter().flatten() {
            target.insert(key.clone(), item.clone());
        }
        serde_json::from_value(value).expect("session")
    }

    #[test]
    fn a_new_session_signs_in_like_the_desktop_client() {
        let fresh = sign_in_url(&session(json!({})), "peer-00112233445566778899aabb").unwrap();
        assert!(fresh.contains("version=2"), "{fresh}");
        assert!(fresh.contains("peer_role=1"), "{fresh}");
        assert!(!fresh.contains("reconnect=1"), "{fresh}");

        let resumed = sign_in_url(
            &session(json!({"signalingReconnect": true})),
            "peer-00112233445566778899aabb",
        )
        .unwrap();
        assert!(resumed.contains("version=3.0"), "{resumed}");
        assert!(resumed.contains("reconnect=1"), "{resumed}");
    }

    #[test]
    fn peer_info_assignment_is_acked_after_the_id_matches() {
        let mut protocol = Protocol::new("peer-00112233445566778899aabb".to_owned());
        let (replies, incoming) = protocol
            .receive(
                r#"{"ackid":7,"peer_info":{"id":3,"name":"peer-00112233445566778899aabb"}}"#,
            )
            .unwrap();
        assert!(incoming.is_none());
        assert_eq!(protocol.peer_id, 3);
        assert_eq!(replies, vec![json!({"ack":7})]);
    }

    #[test]
    fn an_offer_object_inside_peer_msg_is_accepted() {
        let mut protocol = Protocol::new("peer-00112233445566778899aabb".to_owned());
        let (replies, incoming) = protocol
            .receive(
                r#"{"ackid":4,"peer_msg":{"from":1,"to":0,"msg":{"type":"offer","sdp":"v=0"}}}"#,
            )
            .unwrap();
        assert_eq!(replies, vec![json!({"ack":4})]);
        assert!(matches!(incoming, Some(Incoming::Offer(sdp)) if sdp == "v=0"));
    }

    #[test]
    fn bye_is_a_literal_peer_message() {
        let mut protocol = Protocol::new("peer-00112233445566778899aabb".to_owned());
        let packet = protocol.bye();
        assert_eq!(packet["peer_msg"]["msg"], "BYE");
        assert_eq!(packet["peer_msg"]["from"], 0);
    }
}
