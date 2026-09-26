use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Nonce,
};
use sha2::{Digest, Sha256};

const SALT: &[u8] = b"rikka-paste/v1";
/// 配对码只有 40 bit，而指纹会通过 mDNS 公开，因此用较高的内存成本（64 MiB）拖慢离线穷举
const ARGON2_M_COST: u32 = 64 * 1024;
const ARGON2_T_COST: u32 = 3;
const NONCE_LEN: usize = 12;

pub struct Cipher {
    aead: ChaCha20Poly1305,
    /// 密钥指纹，通过 mDNS 公布，用于判断对端是否持有同一配对码
    pub fingerprint: String,
}

impl Cipher {
    pub fn new(sync_key: &str) -> Self {
        let mut key = [0u8; 32];
        let params =
            Params::new(ARGON2_M_COST, ARGON2_T_COST, 1, Some(key.len())).expect("argon2 参数合法");
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(normalize(sync_key).as_bytes(), SALT, &mut key)
            .expect("argon2 参数固定，不会失败");
        let fingerprint = hex(&Sha256::digest(key)[..8]);
        let aead = ChaCha20Poly1305::new_from_slice(&key).expect("密钥长度固定为 32");
        Self { aead, fingerprint }
    }

    /// 输出格式：nonce(12) || ciphertext
    pub fn seal(&self, plain: &[u8]) -> Vec<u8> {
        let nonce_bytes: [u8; NONCE_LEN] = rand::random();
        let ciphertext = self
            .aead
            .encrypt(&Nonce::from(nonce_bytes), plain)
            .expect("加密不会失败");
        [&nonce_bytes[..], &ciphertext].concat()
    }

    pub fn open(&self, data: &[u8]) -> Option<Vec<u8>> {
        let (nonce, ciphertext) = data.split_at_checked(NONCE_LEN)?;
        let nonce: [u8; NONCE_LEN] = nonce.try_into().ok()?;
        self.aead.decrypt(&Nonce::from(nonce), ciphertext).ok()
    }
}

/// 忽略大小写、空白和连字符，方便手动输入配对码
fn normalize(key: &str) -> String {
    key.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(char::to_uppercase)
        .collect()
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_key_mismatch() {
        let a = Cipher::new("abcd-efgh");
        let b = Cipher::new("ABCDEFGH");
        let c = Cipher::new("other");
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_ne!(a.fingerprint, c.fingerprint);

        let sealed = a.seal(b"hello");
        assert_eq!(b.open(&sealed).as_deref(), Some(&b"hello"[..]));
        assert!(c.open(&sealed).is_none());
        assert!(a.open(&sealed[..5]).is_none());
    }
}
