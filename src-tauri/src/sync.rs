//! 同步引擎：轮询本地剪贴板 -> 加密后通过 TCP 发给同配对码的设备；mDNS 负责发现设备。
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
    clipboard::{self, WriteMode},
    config::Config,
    crypto::{self, Cipher},
};

pub const STATE_EVENT: &str = "state-changed";

const SERVICE_TYPE: &str = "_rikkapaste._tcp.local.";
const POLL_INTERVAL: Duration = Duration::from_millis(300);
const IO_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_TEXT_LEN: usize = 4 << 20;
const MAX_FRAME_LEN: usize = MAX_TEXT_LEN + (64 << 10);
const MAX_CLOCK_SKEW_MS: u64 = 5 * 60 * 1000;
const MAX_NAME_LEN: usize = 40;
const LOG_LIMIT: usize = 50;
const PREVIEW_LEN: usize = 80;

#[derive(Serialize, Deserialize)]
struct Message {
    from: String,
    name: String,
    ts: u64,
    text: String,
}

struct Peer {
    id: String,
    name: String,
    addrs: Vec<SocketAddr>,
    fingerprint: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerView {
    id: String,
    name: String,
    addr: String,
    /// 对端配对码与本机一致
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
                matched: p.fingerprint == fingerprint,
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

    fn push_log(&self, incoming: bool, peer: String, text: &str, error: Option<String>) {
        let preview: String = text
            .chars()
            .take(PREVIEW_LEN)
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
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
                clipboard::Read::Text(text) => self.on_local_text(text),
                clipboard::Read::Sensitive | clipboard::Read::Empty => {}
            }
            last_count = count;
        }
    }

    fn on_local_text(self: &Arc<Self>, text: String) {
        if text.is_empty() {
            return;
        }
        if text.len() > MAX_TEXT_LEN {
            self.push_log(
                false,
                "-".into(),
                &text,
                Some("内容超过 4 MB，已跳过".into()),
            );
            return;
        }
        let hash = crypto::sha256(text.as_bytes());
        let (frame, targets) = {
            let mut inner = self.lock();
            if !inner.config.enabled || inner.last_hash == Some(hash) {
                return;
            }
            inner.last_hash = Some(hash);
            let fingerprint = &inner.cipher.fingerprint;
            let targets: Vec<(String, Vec<SocketAddr>)> = inner
                .peers
                .values()
                .filter(|p| &p.fingerprint == fingerprint)
                .map(|p| (p.name.clone(), p.addrs.clone()))
                .collect();
            if targets.is_empty() {
                return;
            }
            let msg = Message {
                from: inner.config.device_id.clone(),
                name: inner.config.device_name.clone(),
                ts: now_ms(),
                text: text.clone(),
            };
            let plain = serde_json::to_vec(&msg).expect("Message 可以序列化");
            (Arc::new(inner.cipher.seal(&plain)), targets)
        };
        let text = Arc::new(text);
        for (name, addrs) in targets {
            let (engine, frame, text) = (self.clone(), frame.clone(), text.clone());
            tauri::async_runtime::spawn(async move {
                let error = send_frame(&addrs, &frame).await.err();
                engine.push_log(false, name, &text, error);
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
                if let Ok(Ok(frame)) = timeout(IO_TIMEOUT, read_frame(stream)).await {
                    engine.on_frame(&frame);
                }
            });
        }
    }

    fn on_frame(&self, frame: &[u8]) {
        let cipher = self.lock().cipher.clone();
        // 解密失败说明配对码不同或数据被篡改，直接丢弃
        let Some(plain) = cipher.open(frame) else {
            return;
        };
        let Ok(msg) = serde_json::from_slice::<Message>(&plain) else {
            return;
        };
        if now_ms().abs_diff(msg.ts) > MAX_CLOCK_SKEW_MS {
            self.push_log(
                true,
                msg.name,
                &msg.text,
                Some("设备时间相差超过 5 分钟，已丢弃".into()),
            );
            return;
        }
        let hash = crypto::sha256(msg.text.as_bytes());
        let mode = {
            let mut inner = self.lock();
            if !inner.config.enabled
                || msg.from == inner.config.device_id
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
        let error = clipboard::write_text(&msg.text, mode).err();
        self.push_log(true, msg.name, &msg.text, error);
    }
}

/// 帧格式：u32 大端长度 || nonce || ciphertext。每条消息一个短连接。
async fn send_frame(addrs: &[SocketAddr], frame: &[u8]) -> Result<(), String> {
    let mut last_error = String::from("没有可用地址");
    for addr in addrs {
        let attempt = async {
            let mut stream = TcpStream::connect(addr).await?;
            stream.write_u32(frame.len() as u32).await?;
            stream.write_all(frame).await?;
            stream.shutdown().await
        };
        match timeout(IO_TIMEOUT, attempt).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(e)) => last_error = format!("{addr}: {e}"),
            Err(_) => last_error = format!("{addr}: 连接超时"),
        }
    }
    Err(last_error)
}

async fn read_frame(mut stream: TcpStream) -> io::Result<Vec<u8>> {
    let len = stream.read_u32().await? as usize;
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "帧过大"));
    }
    let mut buf = vec![0; len];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
