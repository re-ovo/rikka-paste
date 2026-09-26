//! 同步引擎：轮询本地剪贴板（文本/图片） -> 加密后通过 TCP 发给同配对码的设备；mDNS 负责发现设备。
//!
//! 防回环：`last_hash` 记录最近一次已同步内容的 hash。
//! 远端内容写入本地剪贴板后，watcher 会看到变化，但 hash 相同所以不会再发出去。

use std::{
    collections::{HashMap, VecDeque},
    io,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

use crate::{
    clipboard::{self, Content, WriteMode},
    config::Config,
    crypto::{self, Cipher},
};

pub const STATE_EVENT: &str = "state-changed";

const SERVICE_TYPE: &str = "_rikkapaste._tcp.local.";
/// 消息格式的版本，通过 mDNS 公布；版本不同的设备互不发送。未公布版本的旧客户端视为 "1"
const PROTOCOL_VERSION: &str = "2";
const POLL_INTERVAL: Duration = Duration::from_millis(300);
const IO_TIMEOUT: Duration = Duration::from_secs(3);
/// 传输正文时按这个最低吞吐（字节/秒）放宽超时，避免大内容在慢速 Wi-Fi 下被截断
const MIN_THROUGHPUT: u64 = 256 << 10;
const MAX_TEXT_LEN: usize = 4 << 20;
const MAX_IMAGE_LEN: usize = 32 << 20;
const MAX_FRAME_LEN: usize = MAX_IMAGE_LEN + (64 << 10);
const MAX_CLOCK_SKEW_MS: u64 = 5 * 60 * 1000;
const MAX_NAME_LEN: usize = 40;
const LOG_LIMIT: usize = 50;
const PREVIEW_LEN: usize = 80;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Text,
    Png,
}

#[derive(Serialize, Deserialize)]
struct Header {
    from: String,
    name: String,
    ts: u64,
    kind: Kind,
}

struct Peer {
    id: String,
    name: String,
    addrs: Vec<SocketAddr>,
    fingerprint: String,
    version: String,
}

impl Peer {
    fn compatible(&self) -> bool {
        self.version == PROTOCOL_VERSION
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerView {
    id: String,
    name: String,
    addr: String,
    /// 对端协议版本与本机一致
    compatible: bool,
    /// 对端版本兼容且配对码与本机一致
    matched: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    ts: u64,
    incoming: bool,
    peer: String,
    preview: String,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    config: Config,
    fingerprint: String,
    peers: Vec<PeerView>,
    log: Vec<LogEntry>,
}

struct Inner {
    config: Config,
    cipher: Arc<Cipher>,
    /// key: mDNS fullname
    peers: HashMap<String, Peer>,
    last_hash: Option<[u8; 32]>,
    log: VecDeque<LogEntry>,
}

pub struct Engine {
    app: AppHandle,
    config_path: PathBuf,
    port: u16,
    mdns: ServiceDaemon,
    inner: Mutex<Inner>,
}

impl Engine {
    pub fn start(
        app: AppHandle,
        config_path: PathBuf,
    ) -> Result<Arc<Self>, Box<dyn std::error::Error>> {
        let config = Config::load_or_create(&config_path);
        let cipher = Arc::new(Cipher::new(&config.sync_key));
        let listener = tauri::async_runtime::block_on(TcpListener::bind("0.0.0.0:0"))?;
        let port = listener.local_addr()?.port();
        let mdns = ServiceDaemon::new()?;
        let browser = mdns.browse(SERVICE_TYPE)?;

        let engine = Arc::new(Self {
            app,
            config_path,
            port,
            mdns,
            inner: Mutex::new(Inner {
                config,
                cipher,
                peers: HashMap::new(),
                last_hash: None,
                log: VecDeque::new(),
            }),
        });
        engine.announce();

        tauri::async_runtime::spawn(engine.clone().serve(listener));
        let e = engine.clone();
        thread::spawn(move || {
            while let Ok(event) = browser.recv() {
                e.on_mdns_event(event);
            }
        });
        let e = engine.clone();
        thread::spawn(move || e.watch_clipboard());

        Ok(engine)
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn config(&self) -> Config {
        self.lock().config.clone()
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = self.lock();
        let fingerprint = inner.cipher.fingerprint.clone();
        let mut peers: Vec<PeerView> = inner
            .peers
            .values()
            .map(|p| PeerView {
                id: p.id.clone(),
                name: p.name.clone(),
                addr: p.addrs.first().map(ToString::to_string).unwrap_or_default(),
                compatible: p.compatible(),
                matched: p.compatible() && p.fingerprint == fingerprint,
            })
            .collect();
        peers.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        Snapshot {
            config: inner.config.clone(),
            fingerprint,
            peers,
            log: inner.log.iter().cloned().collect(),
        }
    }

    fn notify(&self) {
        let _ = self.app.emit(STATE_EVENT, self.snapshot());
    }

    pub fn update_config(&self, mut config: Config) -> Result<(), String> {
        config.device_name = config
            .device_name
            .trim()
            .chars()
            .take(MAX_NAME_LEN)
            .collect();
        config.sync_key = config.sync_key.trim().to_string();
        if config.device_name.is_empty() {
            return Err("设备名不能为空".into());
        }
        if config.sync_key.is_empty() {
            return Err("配对码不能为空".into());
        }

        let old = self.config();
        config.device_id = old.device_id;
        let cipher =
            (config.sync_key != old.sync_key).then(|| Arc::new(Cipher::new(&config.sync_key)));
        config
            .save(&self.config_path)
            .map_err(|e| format!("保存配置失败: {e}"))?;
        {
            let mut inner = self.lock();
            if let Some(cipher) = cipher {
                inner.cipher = cipher;
            }
            inner.config = config;
        }
        self.announce();
        self.notify();
        Ok(())
    }

    /// 注册（或更新）本机的 mDNS 服务；TXT 里带上设备名和配对码指纹
    fn announce(&self) {
        let (id, name, fingerprint) = {
            let inner = self.lock();
            (
                inner.config.device_id.clone(),
                inner.config.device_name.clone(),
                inner.cipher.fingerprint.clone(),
            )
        };
        let props = HashMap::from([
            ("id".to_string(), id.clone()),
            ("name".to_string(), name),
            ("fp".to_string(), fingerprint),
            ("v".to_string(), PROTOCOL_VERSION.to_string()),
        ]);
        let result = ServiceInfo::new(
            SERVICE_TYPE,
            &id,
            &format!("{id}.local."),
            "",
            self.port,
            props,
        )
        .and_then(|info| self.mdns.register(info.enable_addr_auto()));
        if let Err(e) = result {
            eprintln!("mDNS 注册失败: {e}");
        }
    }

    pub fn shutdown(&self) {
        let fullname = format!("{}.{SERVICE_TYPE}", self.lock().config.device_id);
        if let Ok(status) = self.mdns.unregister(&fullname) {
            let _ = status.recv_timeout(Duration::from_secs(1));
        }
        let _ = self.mdns.shutdown();
    }

    fn on_mdns_event(&self, event: ServiceEvent) {
        match event {
            ServiceEvent::ServiceResolved(info) => {
                let Some(id) = info.get_property_val_str("id") else {
                    return;
                };
                let addrs: Vec<SocketAddr> = info
                    .get_addresses_v4()
                    .into_iter()
                    .map(|ip| SocketAddr::new(ip.into(), info.port))
                    .collect();
                let mut inner = self.lock();
                if id == inner.config.device_id || addrs.is_empty() {
                    return;
                }
                let peer = Peer {
                    id: id.to_string(),
                    name: info.get_property_val_str("name").unwrap_or(id).to_string(),
                    fingerprint: info
                        .get_property_val_str("fp")
                        .unwrap_or_default()
                        .to_string(),
                    version: info.get_property_val_str("v").unwrap_or("1").to_string(),
                    addrs,
                };
                inner.peers.insert(info.fullname.clone(), peer);
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                if self.lock().peers.remove(&fullname).is_none() {
                    return;
                }
            }
            _ => return,
        }
        self.notify();
    }

    fn push_log(&self, incoming: bool, peer: String, preview: String, error: Option<String>) {
        {
            let mut inner = self.lock();
            inner.log.push_front(LogEntry {
                ts: now_ms(),
                incoming,
                peer,
                preview,
                error,
            });
            inner.log.truncate(LOG_LIMIT);
        }
        self.notify();
    }

    fn watch_clipboard(self: Arc<Self>) {
        let mut last_count = clipboard::change_count();
        loop {
            thread::sleep(POLL_INTERVAL);
            let count = clipboard::change_count();
            if count == last_count {
                continue;
            }
            match clipboard::read() {
                clipboard::Read::Busy => continue,
                clipboard::Read::Content(content) => self.on_local_content(content),
                clipboard::Read::Sensitive | clipboard::Read::Empty => {}
            }
            last_count = count;
        }
    }

    fn on_local_content(self: &Arc<Self>, content: Content) {
        let body = content.as_bytes();
        if body.is_empty() {
            return;
        }
        let (kind, limit) = match content {
            Content::Text(_) => (Kind::Text, MAX_TEXT_LEN),
            Content::Png(_) => (Kind::Png, MAX_IMAGE_LEN),
        };
        if body.len() > limit {
            self.push_log(
                false,
                "-".into(),
                preview(&content),
                Some(format!("超过 {} MB，已跳过", limit >> 20)),
            );
            return;
        }
        let hash = crypto::sha256(body);
        let (cipher, header, targets) = {
            let mut inner = self.lock();
            let config = &inner.config;
            if !config.enabled
                || (matches!(kind, Kind::Png) && !config.sync_images)
                || inner.last_hash == Some(hash)
            {
                return;
            }
            inner.last_hash = Some(hash);
            let fingerprint = &inner.cipher.fingerprint;
            let targets: Vec<(String, Vec<SocketAddr>)> = inner
                .peers
                .values()
                .filter(|p| p.compatible() && &p.fingerprint == fingerprint)
                .map(|p| (p.name.clone(), p.addrs.clone()))
                .collect();
            if targets.is_empty() {
                return;
            }
            let header = Header {
                from: inner.config.device_id.clone(),
                name: inner.config.device_name.clone(),
                ts: now_ms(),
                kind,
            };
            (inner.cipher.clone(), header, targets)
        };
        // 图片可达几十 MB，加密放在锁外
        let frame = Arc::new(cipher.seal(&encode_message(&header, body)));
        let preview = preview(&content);
        for (name, addrs) in targets {
            let (engine, frame, preview) = (self.clone(), frame.clone(), preview.clone());
            tauri::async_runtime::spawn(async move {
                let error = send_frame(&addrs, &frame).await.err();
                engine.push_log(false, name, preview, error);
            });
        }
    }

    async fn serve(self: Arc<Self>, listener: TcpListener) {
        loop {
            let stream = match listener.accept().await {
                Ok((stream, _)) => stream,
                Err(e) => {
                    eprintln!("accept 失败: {e}");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };
            let engine = self.clone();
            tauri::async_runtime::spawn(async move {
                if let Ok(frame) = read_frame(stream).await {
                    // 解密和写剪贴板（Windows 上要转码图片）都比较重，不占用异步 worker
                    tauri::async_runtime::spawn_blocking(move || engine.on_frame(&frame));
                }
            });
        }
    }

    fn on_frame(&self, frame: &[u8]) {
        let cipher = self.lock().cipher.clone();
        // 解密失败说明配对码不同或数据被篡改，直接丢弃
        let Some((header, content)) = cipher.open(frame).and_then(decode_message) else {
            return;
        };
        if now_ms().abs_diff(header.ts) > MAX_CLOCK_SKEW_MS {
            self.push_log(
                true,
                header.name,
                preview(&content),
                Some("设备时间相差超过 5 分钟，已丢弃".into()),
            );
            return;
        }
        let hash = crypto::sha256(content.as_bytes());
        let mode = {
            let mut inner = self.lock();
            let config = &inner.config;
            if !config.enabled
                || (matches!(content, Content::Png(_)) && !config.sync_images)
                || header.from == config.device_id
                || inner.last_hash == Some(hash)
            {
                return;
            }
            inner.last_hash = Some(hash);
            if inner.config.record_history {
                WriteMode::Normal
            } else {
                WriteMode::Transient
            }
        };
        let error = clipboard::write(&content, mode).err();
        self.push_log(true, header.name, preview(&content), error);
    }
}

/// 明文格式：u32 大端 header 长度 || header(JSON) || 正文（UTF-8 文本或 PNG 原始字节）
fn encode_message(header: &Header, body: &[u8]) -> Vec<u8> {
    let header = serde_json::to_vec(header).expect("Header 可以序列化");
    [&(header.len() as u32).to_be_bytes()[..], &header, body].concat()
}

fn decode_message(mut plain: Vec<u8>) -> Option<(Header, Content)> {
    let (len, rest) = plain.split_first_chunk::<4>()?;
    let len = u32::from_be_bytes(*len) as usize;
    let header: Header = serde_json::from_slice(rest.get(..len)?).ok()?;
    let body = plain.split_off(4 + len);
    let content = match header.kind {
        Kind::Text if body.len() <= MAX_TEXT_LEN => Content::Text(String::from_utf8(body).ok()?),
        Kind::Png if body.len() <= MAX_IMAGE_LEN => Content::Png(body),
        _ => return None,
    };
    Some((header, content))
}

fn preview(content: &Content) -> String {
    match content {
        Content::Text(text) => text
            .chars()
            .take(PREVIEW_LEN)
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect(),
        Content::Png(png) => {
            let size = format_size(png.len());
            match png_dimensions(png) {
                Some((w, h)) => format!("[图片 {w}×{h} · {size}]"),
                None => format!("[图片 · {size}]"),
            }
        }
    }
}

/// 从 IHDR 读宽高，不需要解码整张图
fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !png.starts_with(SIGNATURE) || png.get(12..16)? != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(png.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(png.get(20..24)?.try_into().ok()?);
    Some((w, h))
}

fn format_size(len: usize) -> String {
    if len < 1 << 20 {
        format!("{} KB", len.div_ceil(1024))
    } else {
        format!("{:.1} MB", len as f64 / (1 << 20) as f64)
    }
}

/// 帧格式：u32 大端长度 || nonce || ciphertext。每条消息一个短连接。
async fn send_frame(addrs: &[SocketAddr], frame: &[u8]) -> Result<(), String> {
    let mut last_error = String::from("没有可用地址");
    for addr in addrs {
        let Ok(connected) = timeout(IO_TIMEOUT, TcpStream::connect(addr)).await else {
            last_error = format!("{addr}: 连接超时");
            continue;
        };
        let attempt = async {
            let mut stream = connected?;
            stream.write_u32(frame.len() as u32).await?;
            stream.write_all(frame).await?;
            stream.shutdown().await
        };
        match timeout(transfer_timeout(frame.len()), attempt).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(e)) => last_error = format!("{addr}: {e}"),
            Err(_) => last_error = format!("{addr}: 发送超时"),
        }
    }
    Err(last_error)
}

/// 正文随数据到达逐步分配，不会因为一个伪造的长度头就预先占用 4 MB
async fn read_frame(mut stream: TcpStream) -> io::Result<Vec<u8>> {
    let len = timeout(IO_TIMEOUT, stream.read_u32()).await?? as usize;
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "帧过大"));
    }
    let mut buf = Vec::new();
    let mut body = stream.take(len as u64);
    timeout(transfer_timeout(len), body.read_to_end(&mut buf)).await??;
    if buf.len() != len {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(buf)
}

fn transfer_timeout(len: usize) -> Duration {
    IO_TIMEOUT + Duration::from_millis(len as u64 * 1000 / MIN_THROUGHPUT)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_roundtrip_and_truncated() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let frame = vec![7u8; 1 << 20];
        let sent = frame.clone();
        let sender = tokio::spawn(async move { send_frame(&[addr], &sent).await });
        let (stream, _) = listener.accept().await.unwrap();
        assert_eq!(read_frame(stream).await.unwrap(), frame);
        sender.await.unwrap().unwrap();

        // 长度头声明 100 字节，实际只发 10 字节就断开
        let sender = tokio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            stream.write_u32(100).await.unwrap();
            stream.write_all(&[0; 10]).await.unwrap();
        });
        let (stream, _) = listener.accept().await.unwrap();
        sender.await.unwrap();
        assert!(read_frame(stream).await.is_err());
    }

    fn header(kind: Kind) -> Header {
        Header {
            from: "a".into(),
            name: "b".into(),
            ts: 1,
            kind,
        }
    }

    #[test]
    fn message_roundtrip() {
        // 引号、换行、控制字符原样传输，不受 JSON 转义影响
        let text = "\"quoted\"\n\u{1}中文";
        let plain = encode_message(&header(Kind::Text), text.as_bytes());
        let Some((h, Content::Text(t))) = decode_message(plain) else {
            panic!()
        };
        assert_eq!((h.from.as_str(), h.name.as_str(), h.ts), ("a", "b", 1));
        assert_eq!(t, text);

        let png = vec![0x89, b'P', 0, 0xff];
        let plain = encode_message(&header(Kind::Png), &png);
        let Some((_, Content::Png(p))) = decode_message(plain) else {
            panic!()
        };
        assert_eq!(p, png);

        // 非 UTF-8 文本、截断的 header 都应拒绝
        assert!(decode_message(encode_message(&header(Kind::Text), &[0xff])).is_none());
        assert!(decode_message(vec![0, 0, 1, 0, b'{']).is_none());
    }

    #[test]
    fn png_preview() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&1920u32.to_be_bytes());
        png.extend_from_slice(&1080u32.to_be_bytes());
        assert_eq!(png_dimensions(&png), Some((1920, 1080)));
        assert_eq!(preview(&Content::Png(png)), "[图片 1920×1080 · 1 KB]");
        assert_eq!(format_size(3 << 20), "3.0 MB");
    }
}
