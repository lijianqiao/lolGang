//! Nostr 临时事件（架构 6.4）：把加密包装进 kind 24810 的事件，经公共中继转发。
//!
//! 实现流程：
//! 1. 每个房间、每次运行随机生成一把 secp256k1 密钥（`NostrKey`），绝不用身份密钥，否则中继能跨房关联同一个人。
//! 2. 事件：标签 `x` 为 room_id 的十六进制（中继按它过滤订阅），content 为包的 Base64。
//!    id = SHA-256(`[0, pubkey, created_at, kind, tags, content]` 的紧凑 JSON)，sig = BIP-340 Schnorr 签名（NIP-01）。
//! 3. 消息：`EVENT` 发布、`REQ` 订阅（按 kind 和 `#x` 过滤）、`CLOSE` 取消订阅。
//! 4. 解析中继回复：只接收 kind 24810、带合法 `x` 标签、content 能解出不超过 1200 字节的事件；其他一律忽略。
//!    不验证 Nostr 层签名：包本身已用房间密钥认证加密，中继也会验签。
//! 5. `OK` 拒绝消息按前缀分类：`rate-limited` 冷却，`blocked` 等则本次运行不再使用该中继。

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use k256::schnorr::{Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::crypto::{self, CryptoError, RoomId, hex};

pub const KIND: u16 = 24810;
const MAX_PACKET_LEN: usize = 1200;
/// 包的 Base64 最多这么长；更长的不用解码就知道超了。
const MAX_CONTENT_LEN: usize = MAX_PACKET_LEN.div_ceil(3) * 4;
/// 中继发来的一条消息最多这么长。装着最大的包的事件约 2.1 KB（Base64 1600 字符，加上 id、公钥、签名、标签），
/// `OK`、`NOTICE` 这些更短；留到 8 KB，超过的不解析直接丢。
const MAX_MESSAGE_LEN: usize = 8 * 1024;

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// 某个房间本次运行用的 Nostr 密钥。
pub struct NostrKey {
    sk: SigningKey,
    pubkey_hex: String,
}

impl NostrKey {
    pub fn generate() -> Result<Self, CryptoError> {
        loop {
            // 32 字节随机数几乎总是合法私钥；不合法（概率约 2^-128）就再取一次
            if let Some(k) = Self::from_secret(&crypto::random_bytes::<32>()?) {
                return Ok(k);
            }
        }
    }

    pub fn from_secret(secret: &[u8; 32]) -> Option<Self> {
        let sk = SigningKey::from_slice(secret).ok()?;
        let pubkey_hex = hex(&sk.verifying_key().to_bytes());
        Some(NostrKey { sk, pubkey_hex })
    }

    pub fn pubkey_hex(&self) -> &str {
        &self.pubkey_hex
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NostrEvent {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u16,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

fn event_id(
    pubkey: &str,
    created_at: u64,
    kind: u16,
    tags: &[Vec<String>],
    content: &str,
) -> [u8; 32] {
    let serial = json!([0, pubkey, created_at, kind, tags, content]).to_string();
    crypto::sha256(&[serial.as_bytes()])
}

pub fn build_event(
    key: &NostrKey,
    room: &RoomId,
    packet: &[u8],
    created_at: u64,
) -> Result<NostrEvent, CryptoError> {
    let aux = crypto::random_bytes::<32>()?;
    Ok(build_event_with_aux(key, room, packet, created_at, &aux))
}

fn build_event_with_aux(
    key: &NostrKey,
    room: &RoomId,
    packet: &[u8],
    created_at: u64,
    aux: &[u8; 32],
) -> NostrEvent {
    let tags = vec![vec!["x".to_string(), hex(room)]];
    let content = B64.encode(packet);
    let id = event_id(&key.pubkey_hex, created_at, KIND, &tags, &content);
    let sig: Signature = key
        .sk
        .sign_raw(&id, aux)
        .expect("对 32 字节摘要签名，失败概率可忽略");
    NostrEvent {
        id: hex(&id),
        pubkey: key.pubkey_hex.clone(),
        created_at,
        kind: KIND,
        tags,
        content,
        sig: hex(&sig.to_bytes()),
    }
}

/// 重算 id 并验 BIP-340 签名（测试和排查用；收包时不验，见模块说明）。
pub fn verify(e: &NostrEvent) -> bool {
    let id = event_id(&e.pubkey, e.created_at, e.kind, &e.tags, &e.content);
    if hex(&id) != e.id {
        return false;
    }
    let (Some(pk), Some(sig)) = (unhex(&e.pubkey), unhex(&e.sig)) else {
        return false;
    };
    let (Ok(vk), Ok(sig)) = (VerifyingKey::from_slice(&pk), Signature::from_slice(&sig)) else {
        return false;
    };
    vk.verify_raw(&id, &sig).is_ok()
}

pub fn event_message(e: &NostrEvent) -> String {
    json!(["EVENT", e]).to_string()
}

pub fn req_message(sub: &str, rooms: &[RoomId], since: u64) -> String {
    let rooms: Vec<String> = rooms.iter().map(|r| hex(r)).collect();
    json!(["REQ", sub, { "kinds": [KIND], "#x": rooms, "since": since }]).to_string()
}

pub fn close_message(sub: &str) -> String {
    json!(["CLOSE", sub]).to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayMsg {
    Event {
        sub: String,
        event_id: String,
        pubkey: String,
        room_id: RoomId,
        packet: Vec<u8>,
    },
    Ok {
        event_id: String,
        accepted: bool,
        message: String,
    },
    Eose,
    Closed(String),
    Notice(String),
    Ignore,
}

fn parse_event(mut a: Vec<Value>) -> Option<RelayMsg> {
    if a.len() < 3 {
        return None;
    }
    // 把事件那一项拿出来用，不复制（swap_remove 只动第 2 项和最后一项，第 1 项还在原处）
    let event = a.swap_remove(2);
    let sub = a.get(1)?.as_str()?.to_string();
    let e: NostrEvent = serde_json::from_value(event).ok()?;
    if e.kind != KIND || e.content.len() > MAX_CONTENT_LEN {
        return None;
    }
    let room_hex = e
        .tags
        .iter()
        .find(|t| t.len() >= 2 && t[0] == "x")?
        .get(1)?;
    let room_id: RoomId = unhex(room_hex)?.try_into().ok()?;
    let packet = B64.decode(&e.content).ok()?;
    if packet.len() > MAX_PACKET_LEN {
        return None;
    }
    Some(RelayMsg::Event {
        sub,
        event_id: e.id,
        pubkey: e.pubkey,
        room_id,
        packet,
    })
}

fn parse_ok(a: &[Value]) -> Option<RelayMsg> {
    Some(RelayMsg::Ok {
        event_id: a.get(1)?.as_str()?.to_string(),
        accepted: a.get(2)?.as_bool()?,
        message: a.get(3).and_then(Value::as_str).unwrap_or("").to_string(),
    })
}

pub fn parse_relay_message(text: &str) -> RelayMsg {
    // 太长的先丢，不解析：坏掉或恶意的中继发来几百 KB 的消息，不该让这个线程把它整份解析成一棵树（3.3.0）
    if text.len() > MAX_MESSAGE_LEN {
        return RelayMsg::Ignore;
    }
    let Ok(Value::Array(a)) = serde_json::from_str::<Value>(text) else {
        return RelayMsg::Ignore;
    };
    let text_at =
        |a: &[Value], i: usize| a.get(i).and_then(Value::as_str).unwrap_or("").to_string();
    let kind = a.first().and_then(Value::as_str).unwrap_or("").to_string();
    let parsed = match kind.as_str() {
        "EVENT" => parse_event(a),
        "OK" => parse_ok(&a),
        "EOSE" => Some(RelayMsg::Eose),
        "CLOSED" => Some(RelayMsg::Closed(text_at(&a, 2))),
        "NOTICE" => Some(RelayMsg::Notice(text_at(&a, 1))),
        _ => None,
    };
    parsed.unwrap_or(RelayMsg::Ignore)
}

/// 中继拒绝事件的原因（`OK` 第三项的前缀，NIP-01）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// 发太快：冷却一会儿再用。
    RateLimited,
    /// 这个中继不收我们的事件：本次运行不再使用。
    Retire,
    /// 其他（比如重复事件、中继自己出错），不影响后续使用。
    Other,
}

pub fn classify_rejection(message: &str) -> Rejection {
    match message.split(':').next().unwrap_or("").trim() {
        "rate-limited" => Rejection::RateLimited,
        "blocked" | "restricted" | "auth-required" | "pow" | "invalid" => Rejection::Retire,
        _ => Rejection::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 参考值由 @noble/curves（独立实现）算出：私钥 32 个 0x01、created_at 1700000000、
    /// room_id 16 个 0x02、包 10 个 0x03、BIP-340 辅助随机数全 0。
    const GOLDEN_PUBKEY: &str = "1b84c5567b126440995d3ed5aaba0565d71e1834604819ff9c17f5e9d5dd078f";
    const GOLDEN_ID: &str = "cbf8ccca1de544abe85b98ae1585af97d52904d6a83c3d7d10a3c30b6f89521c";
    const GOLDEN_SIG: &str = "7afb13b682e3e9fe2e7c01861bd6eb1c5536b8f61d9ef05be4fa19e5ea3884205944d199fd2928f9b8a5ce24142bf60470de4cde5ed7079113ae84eaa864c130";

    fn golden_key() -> NostrKey {
        NostrKey::from_secret(&[1u8; 32]).unwrap()
    }

    #[test]
    fn event_matches_independent_implementation() {
        let key = golden_key();
        assert_eq!(key.pubkey_hex(), GOLDEN_PUBKEY);
        let e = build_event_with_aux(&key, &[2u8; 16], &[3u8; 10], 1_700_000_000, &[0u8; 32]);
        assert_eq!(e.id, GOLDEN_ID);
        assert_eq!(e.sig, GOLDEN_SIG);
        assert_eq!(e.content, "AwMDAwMDAwMDAw==");
    }

    #[test]
    fn random_aux_signatures_still_verify() {
        let key = NostrKey::generate().unwrap();
        let e = build_event(&key, &[2u8; 16], &[9u8; 1200], 1_700_000_000).unwrap();
        assert!(e.content.len() <= 1600);
        assert!(verify(&e));
        let mut bad = e.clone();
        bad.content.replace_range(0..1, "B");
        assert!(!verify(&bad), "改了内容后 id 和签名都对不上");
    }

    #[test]
    fn messages_have_the_expected_shape() {
        let e = build_event_with_aux(
            &golden_key(),
            &[2u8; 16],
            &[3u8; 10],
            1_700_000_000,
            &[0u8; 32],
        );
        let v: serde_json::Value = serde_json::from_str(&event_message(&e)).unwrap();
        assert_eq!(v[0], "EVENT");
        assert_eq!(v[1]["kind"], KIND);
        assert_eq!(v[1]["tags"][0][0], "x");
        assert_eq!(v[1]["tags"][0][1], "02020202020202020202020202020202");

        let v: serde_json::Value =
            serde_json::from_str(&req_message("hs", &[[2u8; 16], [5u8; 16]], 1000)).unwrap();
        assert_eq!(v[0], "REQ");
        assert_eq!(v[1], "hs");
        assert_eq!(v[2]["kinds"][0], KIND);
        assert_eq!(v[2]["#x"].as_array().unwrap().len(), 2);
        assert_eq!(v[2]["since"], 1000);

        assert_eq!(close_message("hs"), r#"["CLOSE","hs"]"#);
    }

    #[test]
    fn parses_relay_replies() {
        let e = build_event_with_aux(
            &golden_key(),
            &[2u8; 16],
            &[3u8; 10],
            1_700_000_000,
            &[0u8; 32],
        );
        let event_json = serde_json::to_string(&e).unwrap();
        match parse_relay_message(&format!(r#"["EVENT","hs",{event_json}]"#)) {
            RelayMsg::Event {
                event_id,
                pubkey,
                room_id,
                packet,
                ..
            } => {
                assert_eq!(
                    (event_id.as_str(), pubkey.as_str()),
                    (GOLDEN_ID, GOLDEN_PUBKEY)
                );
                assert_eq!((room_id, packet), ([2u8; 16], vec![3u8; 10]));
            }
            other => panic!("应解析为 EVENT：{other:?}"),
        }
        assert_eq!(
            parse_relay_message(r#"["OK","abc",false,"rate-limited: slow down"]"#),
            RelayMsg::Ok {
                event_id: "abc".into(),
                accepted: false,
                message: "rate-limited: slow down".into()
            }
        );
        assert_eq!(parse_relay_message(r#"["EOSE","hs"]"#), RelayMsg::Eose);
        assert_eq!(
            parse_relay_message(r#"["NOTICE","hello"]"#),
            RelayMsg::Notice("hello".into())
        );
        assert_eq!(
            parse_relay_message(r#"["CLOSED","hs","auth-required: x"]"#),
            RelayMsg::Closed("auth-required: x".into())
        );
        assert_eq!(parse_relay_message("not json"), RelayMsg::Ignore);
        // 别的 kind、缺 x 标签、内容不是 Base64 的事件都忽略
        let other_kind = event_json.replace("24810", "1");
        assert_eq!(
            parse_relay_message(&format!(r#"["EVENT","hs",{other_kind}]"#)),
            RelayMsg::Ignore
        );
        let no_tag = event_json.replace(r#"[["x","02020202020202020202020202020202"]]"#, "[]");
        assert_eq!(
            parse_relay_message(&format!(r#"["EVENT","hs",{no_tag}]"#)),
            RelayMsg::Ignore
        );
        let bad_content = event_json.replace("AwMDAwMDAwMDAw==", "!!!");
        assert_eq!(
            parse_relay_message(&format!(r#"["EVENT","hs",{bad_content}]"#)),
            RelayMsg::Ignore
        );
    }

    /// 太长的消息不解析就丢；装着最大的包（1200 字节）的事件照收，Base64 再长一组的不用解码就不收（3.3.0）。
    #[test]
    fn long_messages_are_dropped_and_the_largest_packet_still_fits() {
        let big = build_event_with_aux(
            &golden_key(),
            &[2u8; 16],
            &[9u8; MAX_PACKET_LEN],
            1_700_000_000,
            &[0u8; 32],
        );
        let text = format!(r#"["EVENT","hs",{}]"#, serde_json::to_string(&big).unwrap());
        assert!(
            text.len() < MAX_MESSAGE_LEN / 2,
            "最大的事件 {} 字节，离上限很远",
            text.len()
        );
        assert!(matches!(
            parse_relay_message(&text),
            RelayMsg::Event { packet, .. } if packet.len() == MAX_PACKET_LEN
        ));
        let mut over = big.clone();
        over.content.push_str("AAAA");
        let text = format!(
            r#"["EVENT","hs",{}]"#,
            serde_json::to_string(&over).unwrap()
        );
        assert_eq!(parse_relay_message(&text), RelayMsg::Ignore);

        let ok_with = |n: usize| format!(r#"["OK","abc",true,"{}"]"#, "x".repeat(n));
        let at_limit = ok_with(MAX_MESSAGE_LEN - ok_with(0).len());
        assert_eq!(at_limit.len(), MAX_MESSAGE_LEN);
        assert!(matches!(
            parse_relay_message(&at_limit),
            RelayMsg::Ok { accepted: true, .. }
        ));
        let over_limit = ok_with(MAX_MESSAGE_LEN - ok_with(0).len() + 1);
        assert_eq!(parse_relay_message(&over_limit), RelayMsg::Ignore);
    }

    /// 随机输入（见 `fuzz.rs`）：中继发来的每种消息随机弄坏，解析和拒绝原因的分类都不能 panic。
    #[test]
    fn broken_relay_messages_never_panic() {
        let event = |packet: &[u8]| {
            let e =
                build_event_with_aux(&golden_key(), &[2u8; 16], packet, 1_700_000_000, &[0u8; 32]);
            format!(r#"["EVENT","hs",{}]"#, serde_json::to_string(&e).unwrap()).into_bytes()
        };
        let samples = vec![
            event(&[3u8; 10]),
            event(&[9u8; MAX_PACKET_LEN]),
            br#"["OK","abc",false,"rate-limited: slow down"]"#.to_vec(),
            br#"["CLOSED","hs","auth-required: x"]"#.to_vec(),
            br#"["NOTICE","hello"]"#.to_vec(),
            br#"["EOSE","hs"]"#.to_vec(),
        ];
        crate::fuzz::run("中继消息", &samples, |b| {
            let text = String::from_utf8_lossy(b);
            match parse_relay_message(&text) {
                RelayMsg::Ok { message, .. } | RelayMsg::Closed(message) => {
                    let _ = classify_rejection(&message);
                }
                RelayMsg::Event { packet, .. } => assert!(packet.len() <= MAX_PACKET_LEN),
                _ => {}
            }
        });
    }

    /// 中继上谁都能发东西：一条消息（最多 256 KB）嵌套到最深、类型全不对、房间编号不是十六进制，都只是忽略，不爆栈。
    #[test]
    fn hostile_relay_messages_are_ignored() {
        let max = 256 << 10;
        for deep in [
            "[".repeat(max),
            format!("[\"EVENT\",\"hs\",{}", "{\"a\":".repeat(max / 5)),
        ] {
            assert_eq!(parse_relay_message(&deep), RelayMsg::Ignore);
        }
        for text in [
            r#"["EVENT"]"#,
            r#"["EVENT",1,{}]"#,
            r#"["EVENT","hs",[]]"#,
            r#"["EVENT","hs",{"kind":"x","tags":7,"content":null}]"#,
            r#"["OK"]"#,
            r#"["OK",1,"yes"]"#,
            r#"[null,"OK"]"#,
            "[]",
            "",
        ] {
            assert_eq!(parse_relay_message(text), RelayMsg::Ignore, "{text}");
        }
        let e = build_event_with_aux(
            &golden_key(),
            &[2u8; 16],
            &[3u8; 10],
            1_700_000_000,
            &[0u8; 32],
        );
        let json = serde_json::to_string(&e).unwrap();
        for room in [
            "0202",
            "zz02020202020202020202020202020202",
            "字字0202020202020202020202020202",
        ] {
            let moved = json.replace("02020202020202020202020202020202", room);
            assert_eq!(
                parse_relay_message(&format!(r#"["EVENT","hs",{moved}]"#)),
                RelayMsg::Ignore,
                "{room}"
            );
        }
    }

    #[test]
    fn classifies_ok_messages() {
        assert_eq!(
            classify_rejection("rate-limited: too fast"),
            Rejection::RateLimited
        );
        for m in [
            "blocked: no",
            "restricted: members only",
            "auth-required: login",
            "pow: 20 bits",
            "invalid: bad",
        ] {
            assert_eq!(classify_rejection(m), Rejection::Retire, "{m}");
        }
        assert_eq!(classify_rejection("error: database"), Rejection::Other);
        assert_eq!(
            classify_rejection("duplicate: already have it"),
            Rejection::Other
        );
    }
}
