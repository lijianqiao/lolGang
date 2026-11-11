//! 加密原语：哈希、房间密钥派生、包加解密、身份签名。
//!
//! 实现流程：
//! 1. `sha256`：事件 ID、房间码校验位、owner_tag 都用 SHA-256。
//! 2. `derive_room_keys`：用 HKDF 从房间码里的 room_secret 和 owner_tag 派生两样东西（架构第 3 节）：
//!    包加密密钥 room_key，以及房间标识 room_id。HKDF 是把一段高熵密钥材料拆成多把子密钥的标准做法，
//!    info 字符串不同，得到的子密钥互不相关；owner_tag 作为 salt 参与派生，换掉它就是另一个房间。
//! 3. `seal` / `open`：XChaCha20-Poly1305 加解密。它是带认证的加密（AEAD）：密文或关联数据被改动
//!    任何一位，解密都会失败。nonce 是 24 字节随机数，多人共用一把密钥也不必担心重复。
//! 4. `Identity`：Ed25519 身份密钥。私钥是 32 字节随机数，公钥就是 memberId。
//!    签名对象是 "hs/sig/v1" ‖ event_id，加前缀是为了让这把密钥签的东西不会被挪作他用。
//! 5. `random_bytes`：直接向操作系统要随机数。

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};

pub type RoomKey = [u8; 32];
pub type RoomId = [u8; 16];
pub type MemberId = [u8; 32];
pub type EventId = [u8; 32];

#[derive(Debug, PartialEq, Eq)]
pub enum CryptoError {
    Random,
    Open,
}

// 会经 AppError 显示给用户，按界面语言出（3.3.2）
impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CryptoError::Random => crate::tr!("系统随机数不可用", "系统随机数不可用"),
            CryptoError::Open => crate::tr!(
                "解密失败：密钥不对或数据被改动",
                "解密失败：密钥不对或数据被改动"
            ),
        })
    }
}

impl std::error::Error for CryptoError {}

/// 依次喂入多段数据，返回 SHA-256 摘要。
pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

pub fn random_bytes<const N: usize>() -> Result<[u8; N], CryptoError> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|_| CryptoError::Random)?;
    Ok(b)
}

pub fn derive_room_keys(room_secret: &[u8; 16], owner_tag: &[u8; 10]) -> (RoomKey, RoomId) {
    let hk = Hkdf::<Sha256>::new(Some(owner_tag), room_secret);
    let mut key = [0u8; 32];
    let mut id = [0u8; 16];
    // 32 和 16 字节远小于 HKDF-SHA256 的输出上限（255 × 32 字节），expand 不会失败
    hk.expand(b"hs/room-key/v1", &mut key).expect("长度合法");
    hk.expand(b"hs/room-id/v1", &mut id).expect("长度合法");
    (key, id)
}

/// GT3 队伍码（3.3.0，架构 3.1）：从码的主体 `C` 派生包加密密钥和房间标识。
/// salt 和 info 都和上面 GT2 的不同，两种码不可能派生出同一个房间。
pub fn derive_team_keys_v3(commit: &[u8; 12]) -> (RoomKey, RoomId) {
    let hk = Hkdf::<Sha256>::new(Some(b"gang/team/v3"), commit);
    let mut key = [0u8; 32];
    let mut id = [0u8; 16];
    hk.expand(b"gang/room-key/v3", &mut key).expect("长度合法");
    hk.expand(b"gang/room-id/v3", &mut id).expect("长度合法");
    (key, id)
}

pub fn seal(key: &RoomKey, nonce: &[u8; 24], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new(&Key::from(*key));
    cipher
        .encrypt(
            &XNonce::from(*nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("明文远小于算法上限，加密不会失败")
}

pub fn open(
    key: &RoomKey,
    nonce: &[u8; 24],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = XChaCha20Poly1305::new(&Key::from(*key));
    cipher
        .decrypt(
            &XNonce::from(*nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Open)
}

/// 本机身份。私钥只在这个结构体里，落盘加密在 storage 里做（M1-2）。
pub struct Identity {
    sk: SigningKey,
}

impl Identity {
    pub fn generate() -> Result<Self, CryptoError> {
        Ok(Self::from_secret(&random_bytes::<32>()?))
    }

    pub fn from_secret(secret: &[u8; 32]) -> Self {
        Self {
            sk: SigningKey::from_bytes(secret),
        }
    }

    pub fn secret(&self) -> [u8; 32] {
        self.sk.to_bytes()
    }

    pub fn member_id(&self) -> MemberId {
        self.sk.verifying_key().to_bytes()
    }

    pub fn sign_event_id(&self, event_id: &EventId) -> [u8; 64] {
        self.sk.sign(&sig_message(event_id)).to_bytes()
    }
}

fn sig_message(event_id: &EventId) -> Vec<u8> {
    [b"hs/sig/v1".as_slice(), event_id].concat()
}

/// 公钥不合法或签名不对都返回 false。用 verify_strict 拒绝可塑签名。
pub fn verify_event_sig(member_id: &MemberId, event_id: &EventId, sig: &[u8; 64]) -> bool {
    let Ok(vk) = VerifyingKey::from_bytes(member_id) else {
        return false;
    };
    vk.verify_strict(&sig_message(event_id), &Signature::from_bytes(sig))
        .is_ok()
}

/// 字节 → 小写十六进制：公钥、表情图的哈希、透明窗口里队友的短编号都这样写（3.1.1 起只有这一份）。
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 指纹摘要：SHA-256(公钥) 前 8 字节的 Base32，共 13 个字符。
pub fn fingerprint(member_id: &MemberId) -> String {
    crate::base32::encode(&sha256(&[member_id])[..8], 64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_answer() {
        assert_eq!(
            hex(&sha256(&[b"hello ", b"world"])),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn room_keys_golden_and_owner_tag_matters() {
        let (key, id) = derive_room_keys(&[1u8; 16], &[2u8; 10]);
        assert_eq!(hex(&key), GOLDEN_ROOM_KEY);
        assert_eq!(hex(&id), GOLDEN_ROOM_ID);
        let (key2, id2) = derive_room_keys(&[1u8; 16], &[3u8; 10]);
        assert_ne!(key, key2);
        assert_ne!(id, id2);
    }

    #[test]
    fn seal_open_roundtrip_and_tamper() {
        let key = [9u8; 32];
        let nonce = [4u8; 24];
        let ct = seal(&key, &nonce, b"header", b"hello");
        assert_eq!(open(&key, &nonce, b"header", &ct), Ok(b"hello".to_vec()));
        assert_eq!(open(&key, &nonce, b"HEADER", &ct), Err(CryptoError::Open));
        let mut bad = ct.clone();
        bad[0] ^= 1;
        assert_eq!(open(&key, &nonce, b"header", &bad), Err(CryptoError::Open));
        assert_eq!(
            open(&[8u8; 32], &nonce, b"header", &ct),
            Err(CryptoError::Open)
        );
    }

    #[test]
    fn sign_and_verify() {
        let me = Identity::from_secret(&[7u8; 32]);
        let id = [5u8; 32];
        let sig = me.sign_event_id(&id);
        assert!(verify_event_sig(&me.member_id(), &id, &sig));
        assert!(!verify_event_sig(&me.member_id(), &[6u8; 32], &sig));
        let other = Identity::from_secret(&[8u8; 32]);
        assert!(!verify_event_sig(&other.member_id(), &id, &sig));
    }

    #[test]
    fn generated_identities_differ() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        assert_ne!(a.member_id(), b.member_id());
        assert_eq!(
            Identity::from_secret(&a.secret()).member_id(),
            a.member_id()
        );
    }

    #[test]
    fn fingerprint_is_13_chars() {
        let me = Identity::from_secret(&[7u8; 32]);
        assert_eq!(fingerprint(&me.member_id()).len(), 13);
    }

    // 由本实现算出、用于防止以后无意中改变派生方式
    const GOLDEN_ROOM_KEY: &str =
        "3ab2cad702090aabb1bb3ffbbac39488b2d5b4dd0a815deb969a84b30e2e04d8";
    const GOLDEN_ROOM_ID: &str = "914010ee40e2e6d197fcf5dfba50856b";
}
