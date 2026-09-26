use std::{fs, io, path::Path};

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub device_id: String,
    pub device_name: String,
    /// 配对码：持有相同配对码的设备才会互相同步
    pub sync_key: String,
    pub enabled: bool,
    /// 远端同步来的内容是否进入本机剪贴板历史（Maccy / Win+V）
    pub record_history: bool,
    pub sync_images: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_id: format!("{:016x}", rand::random::<u64>()),
            device_name: gethostname::gethostname().to_string_lossy().into_owned(),
            sync_key: generate_key(),
            enabled: true,
            record_history: true,
            sync_images: true,
        }
    }
}

impl Config {
    pub fn load_or_create(path: &Path) -> Self {
        if let Some(config) = fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            return config;
        }
        let config = Self::default();
        if let Err(e) = config.save(path) {
            eprintln!("保存配置失败: {e}");
        }
        config
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, serde_json::to_vec_pretty(self)?)
    }
}

/// 生成形如 `K7QM-2XPA` 的配对码（40 bit）
pub fn generate_key() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    (0..8)
        .map(|i| {
            let c = ALPHABET[rand::random_range(0..ALPHABET.len())] as char;
            if i > 0 && i % 4 == 0 {
                format!("-{c}")
            } else {
                c.to_string()
            }
        })
        .collect()
}
