//! 本机主密钥与本机数据加密（架构 9.3）。
//!
//! 实现流程：
//! 1. 第一次运行时生成 32 字节随机主密钥，存进 Windows 凭据管理器（服务名 lolgang，用户名 master-key）。
//!    凭据管理器由系统用当前 Windows 账户保护（底层就是 DPAPI），换账户或换电脑都读不到。
//! 2. 之后每次启动从凭据管理器取回同一把主密钥。
//! 3. 私钥、房间码这类本机敏感数据用主密钥加密后存进 SQLite：XChaCha20-Poly1305，随机 nonce 放在密文前面。
//!    关联数据填用途（如 "hs/local/identity"），把一种密文当另一种用途解密会失败。
//! 4. 测试用 `MemoryKeyStore`，不碰真实的凭据管理器。

use crate::crypto;
use crate::error::{AppError, AppResult};

pub const PURPOSE_IDENTITY: &[u8] = b"hs/local/identity";
pub const PURPOSE_ROOM_CODE: &[u8] = b"hs/local/room-code";
const USER: &str = "master-key";

pub trait KeyStore {
    fn master_key(&self) -> AppResult<[u8; 32]>;
}

/// Windows 凭据管理器。正式使用时 `service` 固定为 "lolgang"。
pub struct SystemKeyStore {
    pub service: &'static str,
}

pub const APP_KEYSTORE: SystemKeyStore = SystemKeyStore { service: "lolgang" };

fn keystore_err(e: keyring::Error) -> AppError {
    AppError::KeyStore(e.to_string())
}

impl KeyStore for SystemKeyStore {
    fn master_key(&self) -> AppResult<[u8; 32]> {
        let entry = keyring::Entry::new(self.service, USER).map_err(keystore_err)?;
        match entry.get_secret() {
            Ok(bytes) => bytes.try_into().map_err(|_| {
                AppError::KeyStore(
                    crate::tr!(
                        "凭据管理器里的主密钥长度不对",
                        "The master key in Credential Manager has the wrong length"
                    )
                    .into(),
                )
            }),
            Err(keyring::Error::NoEntry) => {
                let key = crypto::random_bytes::<32>()?;
                entry.set_secret(&key).map_err(keystore_err)?;
                Ok(key)
            }
            Err(e) => Err(keystore_err(e)),
        }
    }
}

#[cfg(test)]
pub struct MemoryKeyStore(pub [u8; 32]);

#[cfg(test)]
impl KeyStore for MemoryKeyStore {
    fn master_key(&self) -> AppResult<[u8; 32]> {
        Ok(self.0)
    }
}

/// 返回 nonce（24 字节）‖ 密文。
pub fn encrypt_local(key: &[u8; 32], purpose: &[u8], plaintext: &[u8]) -> AppResult<Vec<u8>> {
    let nonce = crypto::random_bytes::<24>()?;
    let mut out = nonce.to_vec();
    out.extend(crypto::seal(key, &nonce, purpose, plaintext));
    Ok(out)
}

pub fn decrypt_local(key: &[u8; 32], purpose: &[u8], data: &[u8]) -> AppResult<Vec<u8>> {
    let (nonce, ct) = data.split_first_chunk::<24>().ok_or(AppError::Decrypt)?;
    crypto::open(key, nonce, purpose, ct).map_err(|_| AppError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_encryption_roundtrip_and_purpose_binding() {
        let key = [3u8; 32];
        let ct = encrypt_local(&key, PURPOSE_IDENTITY, b"secret").unwrap();
        assert!(!ct.windows(6).any(|w| w == b"secret"));
        assert_eq!(
            decrypt_local(&key, PURPOSE_IDENTITY, &ct).unwrap(),
            b"secret".to_vec()
        );
        assert!(matches!(
            decrypt_local(&key, PURPOSE_ROOM_CODE, &ct),
            Err(AppError::Decrypt)
        ));
        assert!(matches!(
            decrypt_local(&[4u8; 32], PURPOSE_IDENTITY, &ct),
            Err(AppError::Decrypt)
        ));
        assert!(matches!(
            decrypt_local(&key, PURPOSE_IDENTITY, &ct[..10]),
            Err(AppError::Decrypt)
        ));
    }

    #[test]
    fn memory_store_returns_its_key() {
        assert_eq!(MemoryKeyStore([5u8; 32]).master_key().unwrap(), [5u8; 32]);
    }

    /// 会在 Windows 凭据管理器里建一个测试条目再删掉。平时不跑，手动验证时用：
    /// cargo test --lib keystore -- --ignored
    #[test]
    #[ignore]
    fn system_store_creates_then_reuses_key() {
        let store = SystemKeyStore {
            service: "lolgang-selftest",
        };
        let first = store.master_key().unwrap();
        let second = store.master_key().unwrap();
        assert_eq!(first, second);
        keyring::Entry::new("lolgang-selftest", USER)
            .unwrap()
            .delete_credential()
            .unwrap();
    }
}
