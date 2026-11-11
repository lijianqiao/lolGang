//! 队伍码（1.x 的房间码，2.0 换了前缀和校验域，和 1.x 的群码区分开）。
//!
//! 实现流程：
//! 1. 组成：room_secret（16 字节随机数）‖ owner_tag（房主公钥 SHA-256 的前 10 字节）= payload 26 字节；
//!    check = SHA-256("gang/team/v2" ‖ payload) 的前 10 位。
//! 2. 编码：payload ‖ check 共 218 位，用 Crockford Base32 编成 44 个字符，前面加 "GT2"。
//!    展示时每 4 个字符一组，用连字符隔开：GT2-XXXX-XXXX-…（不算连字符共 48 个字符）。
//! 3. 解析：去掉连字符和空白、转大写，检查 "GT2" 前缀和长度，解码后重算校验位。
//!    校验位的作用：抄错时立刻报错，而不是派生出另一把「有效」密钥、进入一个永远没人的空房间。
//!    10 位校验能拦下约 99.9% 的单字符错误（每种错误约有 1/1024 的概率漏过）。
//! 4. 派生：`keys()` 交给 crypto::derive_room_keys 得到 room_key 和 room_id。
//! 5. GT3（3.3.0，架构 3.1）：新建的队伍用 20 个字符的短码。上面那种长，是因为「钥匙」（128 位）和「房主凭证」（80 位）是两段；
//!    GT3 合成一段：建队时取 16 字节随机 salt，主体 C = SHA-256("gang/team/v3" ‖ 房主公钥 ‖ salt) 的前 95 位，加 5 位校验。
//!    钥匙从 C 派生；salt 放进建队事件（在加密包里，只有队里的人看得到），队友用「作者公钥 + salt」重算 C 来认房主（`OwnerRule`）。
//!    外人不知道 salt，C 对他们是随机数；队里的人要冒充房主，得找到另一对（公钥、salt）算出同一个 C（95 位的第二原像）。
//!    `TeamCode` 两种都认：GT2 的队伍照常用，这个文件里 GT2 的代码一行没动。

use crate::crypto::{self, MemberId, RoomId, RoomKey};

const PREFIX: &str = "GT2";
const PAYLOAD_LEN: usize = 26;
const NBITS: usize = PAYLOAD_LEN * 8 + 10;

#[derive(Debug, PartialEq, Eq)]
pub enum RoomCodeError {
    BadPrefix,
    BadEncoding,
    BadCheck,
}

// 进队时显示给用户，按界面语言出（3.3.2）
impl std::fmt::Display for RoomCodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RoomCodeError::BadPrefix => {
                crate::tr!(
                    "队伍码应以 GT3 或 GT2 开头",
                    "A team code starts with GT3 or GT2"
                )
            }
            RoomCodeError::BadEncoding => {
                crate::tr!(
                    "队伍码长度或字符不对",
                    "The team code has the wrong length or characters"
                )
            }
            RoomCodeError::BadCheck => crate::tr!(
                "队伍码校验位不对，请检查是否抄错",
                "The team code's check digit is wrong. Check it was copied correctly"
            ),
        })
    }
}

impl std::error::Error for RoomCodeError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoomCode {
    pub room_secret: [u8; 16],
    pub owner_tag: [u8; 10],
}

/// owner_tag = SHA-256(房主公钥) 的前 10 字节，也是房主信任根（架构第 3 节）。
pub fn owner_tag_of(member_id: &MemberId) -> [u8; 10] {
    let h = crypto::sha256(&[member_id]);
    let mut tag = [0u8; 10];
    tag.copy_from_slice(&h[..10]);
    tag
}

fn check_bits(payload: &[u8; PAYLOAD_LEN]) -> u16 {
    let h = crypto::sha256(&[b"gang/team/v2", payload]);
    (u16::from(h[0]) << 2) | u16::from(h[1] >> 6)
}

impl RoomCode {
    pub fn format(&self) -> String {
        let mut payload = [0u8; PAYLOAD_LEN];
        payload[..16].copy_from_slice(&self.room_secret);
        payload[16..].copy_from_slice(&self.owner_tag);
        let check = check_bits(&payload);
        let mut data = payload.to_vec();
        data.push((check >> 2) as u8);
        data.push(((check & 0b11) << 6) as u8);
        let body = crate::base32::encode(&data, NBITS);
        let groups: Vec<&str> = body
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).expect("Base32 输出是 ASCII"))
            .collect();
        format!("{PREFIX}-{}", groups.join("-"))
    }

    pub fn parse(s: &str) -> Result<Self, RoomCodeError> {
        let cleaned: String = s
            .chars()
            .filter(|c| *c != '-' && !c.is_whitespace())
            .collect::<String>()
            .to_ascii_uppercase();
        let rest = cleaned
            .strip_prefix(PREFIX)
            .ok_or(RoomCodeError::BadPrefix)?;
        let data = crate::base32::decode(rest, NBITS).ok_or(RoomCodeError::BadEncoding)?;
        let mut payload = [0u8; PAYLOAD_LEN];
        payload.copy_from_slice(&data[..PAYLOAD_LEN]);
        let check = (u16::from(data[PAYLOAD_LEN]) << 2) | u16::from(data[PAYLOAD_LEN + 1] >> 6);
        if check != check_bits(&payload) {
            return Err(RoomCodeError::BadCheck);
        }
        let mut code = RoomCode {
            room_secret: [0; 16],
            owner_tag: [0; 10],
        };
        code.room_secret.copy_from_slice(&payload[..16]);
        code.owner_tag.copy_from_slice(&payload[16..]);
        Ok(code)
    }

    pub fn keys(&self) -> (RoomKey, RoomId) {
        crypto::derive_room_keys(&self.room_secret, &self.owner_tag)
    }
}

// ---------- GT3：短队伍码（3.3.0，文件头第 5 条） ----------

const PREFIX_V3: &str = "GT3";
/// 主体 95 位 + 校验 5 位 = 20 个字符。
const NBITS_V3: usize = 100;

/// GT3 队伍码。主体 `C` 放在 12 字节里，只用前 95 位，最后一位总是 0。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShortCode {
    pub commit: [u8; 12],
}

/// `C` = SHA-256("gang/team/v3" ‖ 房主公钥 ‖ salt) 的前 95 位。
/// salt 是 16 字节随机数、只有队里的人知道，所以 `C` 对外人是随机的；队里的人要冒充房主，得找到另一对（公钥、salt）算出同一个 `C`。
pub fn commit_of(owner: &MemberId, salt: &[u8; 16]) -> [u8; 12] {
    let h = crypto::sha256(&[b"gang/team/v3", owner, salt]);
    let mut c = [0u8; 12];
    c.copy_from_slice(&h[..12]);
    c[11] &= 0xfe;
    c
}

/// 存进 rooms 表、用来认房主的值。由 `C` 单向算出来：`C` 是钥匙的种子，不能明文存，这个可以。
pub fn owner_check_of(commit: &[u8; 12]) -> [u8; 32] {
    crypto::sha256(&[b"gang/team-owner/v3", commit])
}

fn check_bits_v3(commit: &[u8; 12]) -> u8 {
    crypto::sha256(&[b"gang/team-check/v3", commit])[0] >> 3
}

impl ShortCode {
    pub fn new(owner: &MemberId, salt: &[u8; 16]) -> Self {
        ShortCode {
            commit: commit_of(owner, salt),
        }
    }

    pub fn format(&self) -> String {
        let check = check_bits_v3(&self.commit);
        // 第 95 位（第 12 个字节的最低位）是校验的最高位，后面 4 位放在第 13 个字节的高半边
        let mut data = self.commit.to_vec();
        data[11] |= check >> 4;
        data.push((check & 0x0f) << 4);
        let body = crate::base32::encode(&data, NBITS_V3);
        let groups: Vec<&str> = body
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).expect("Base32 输出是 ASCII"))
            .collect();
        format!("{PREFIX_V3}-{}", groups.join("-"))
    }

    /// `rest` 是去掉连字符、空白、转成大写、去掉 "GT3" 之后的部分。
    fn parse_body(rest: &str) -> Result<Self, RoomCodeError> {
        let data = crate::base32::decode(rest, NBITS_V3).ok_or(RoomCodeError::BadEncoding)?;
        let (Some(head), Some(&last)) = (data.get(..12), data.get(12)) else {
            return Err(RoomCodeError::BadEncoding);
        };
        let mut commit = [0u8; 12];
        commit.copy_from_slice(head);
        let check = ((commit[11] & 1) << 4) | (last >> 4);
        commit[11] &= 0xfe;
        if check != check_bits_v3(&commit) {
            return Err(RoomCodeError::BadCheck);
        }
        Ok(ShortCode { commit })
    }

    pub fn keys(&self) -> (RoomKey, RoomId) {
        crypto::derive_team_keys_v3(&self.commit)
    }
}

/// 怎么认房主。存在 rooms 表里（`owner_tag` 那一列，按长度区分），不是秘密。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerRule {
    /// GT2：房主公钥 SHA-256 的前 10 字节。
    Tag([u8; 10]),
    /// GT3：`owner_check_of(C)`。建队事件的作者公钥和内容里的 salt 重算出的 `C` 要对得上它。
    Commit([u8; 32]),
}

impl OwnerRule {
    /// 这条建队事件（作者、内容里的 salt）是不是房主发的。GT3 里没带 salt 的一律不算。
    pub fn admits(&self, author: &MemberId, salt: Option<&[u8; 16]>) -> bool {
        match self {
            OwnerRule::Tag(tag) => owner_tag_of(author) == *tag,
            OwnerRule::Commit(check) => {
                salt.is_some_and(|s| owner_check_of(&commit_of(author, s)) == *check)
            }
        }
    }

    /// 手里没有建队事件时，`member` 是不是房主：GT2 看公钥摘要；GT3 只能看推导时记下的房主（`rooms.owner_id`）。
    pub fn is_owner(&self, member: &MemberId, known_owner: Option<&MemberId>) -> bool {
        match self {
            OwnerRule::Tag(tag) => owner_tag_of(member) == *tag,
            OwnerRule::Commit(_) => known_owner == Some(member),
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            OwnerRule::Tag(t) => t,
            OwnerRule::Commit(c) => c,
        }
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        match b.len() {
            10 => b.try_into().ok().map(OwnerRule::Tag),
            32 => b.try_into().ok().map(OwnerRule::Commit),
            _ => None,
        }
    }
}

/// 两种队伍码。新建的队伍用 GT3；GT2 是 3.2.0 及以前建的，永远还认。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeamCode {
    V2(RoomCode),
    V3(ShortCode),
}

impl TeamCode {
    pub fn parse(s: &str) -> Result<Self, RoomCodeError> {
        let cleaned: String = s
            .chars()
            .filter(|c| *c != '-' && !c.is_whitespace())
            .collect::<String>()
            .to_ascii_uppercase();
        match cleaned.strip_prefix(PREFIX_V3) {
            Some(rest) => ShortCode::parse_body(rest).map(TeamCode::V3),
            None => RoomCode::parse(&cleaned).map(TeamCode::V2),
        }
    }

    pub fn format(&self) -> String {
        match self {
            TeamCode::V2(c) => c.format(),
            TeamCode::V3(c) => c.format(),
        }
    }

    pub fn keys(&self) -> (RoomKey, RoomId) {
        match self {
            TeamCode::V2(c) => c.keys(),
            TeamCode::V3(c) => c.keys(),
        }
    }

    pub fn owner_rule(&self) -> OwnerRule {
        match self {
            TeamCode::V2(c) => OwnerRule::Tag(c.owner_tag),
            TeamCode::V3(c) => OwnerRule::Commit(owner_check_of(&c.commit)),
        }
    }

    /// 本机加密存库用的字节：GT2 是 26 字节（secret ‖ owner_tag），GT3 是 12 字节（`C`）。
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            TeamCode::V2(c) => [c.room_secret.as_slice(), &c.owner_tag].concat(),
            TeamCode::V3(c) => c.commit.to_vec(),
        }
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        match b.len() {
            26 => Some(TeamCode::V2(RoomCode {
                room_secret: b[..16].try_into().ok()?,
                owner_tag: b[16..].try_into().ok()?,
            })),
            12 => {
                let mut commit: [u8; 12] = b.try_into().ok()?;
                commit[11] &= 0xfe;
                Some(TeamCode::V3(ShortCode { commit }))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RoomCode {
        RoomCode {
            room_secret: [0x11; 16],
            owner_tag: [0x22; 10],
        }
    }

    #[test]
    fn format_shape() {
        let s = sample().format();
        assert!(s.starts_with("GT2-"));
        assert_eq!(s.chars().filter(|c| *c != '-').count(), 3 + 44); // "GT2" + 44 个 Base32 字符
        assert_eq!(s, GOLDEN_CODE);
    }

    #[test]
    fn roundtrip_ignores_case_hyphens_spaces() {
        let s = sample().format();
        assert_eq!(RoomCode::parse(&s), Ok(sample()));
        let messy = format!("  {} ", s.to_lowercase().replace('-', " "));
        assert_eq!(RoomCode::parse(&messy), Ok(sample()));
    }

    #[test]
    fn rejects_prefix_length_and_check() {
        let s = sample().format();
        assert_eq!(
            RoomCode::parse(&s.replacen("GT2", "HS1", 1)),
            Err(RoomCodeError::BadPrefix)
        );
        assert_eq!(
            RoomCode::parse(&s[..s.len() - 1]),
            Err(RoomCodeError::BadEncoding)
        );
        // 把第 10 个字符换成别的字符
        let mut chars: Vec<char> = s.chars().collect();
        chars[10] = if chars[10] == 'A' { 'B' } else { 'A' };
        let typo: String = chars.into_iter().collect();
        assert_eq!(RoomCode::parse(&typo), Err(RoomCodeError::BadCheck));
    }

    #[test]
    fn single_char_typos_almost_always_rejected() {
        let s = sample().format();
        let alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
        let (mut tried, mut accepted) = (0, 0);
        for (i, orig) in s.char_indices().skip(4).filter(|(_, c)| *c != '-') {
            for c in alphabet.chars().filter(|c| *c != orig) {
                let mut t = s.clone();
                t.replace_range(i..i + 1, &c.to_string());
                tried += 1;
                if RoomCode::parse(&t).is_ok() {
                    accepted += 1;
                }
            }
        }
        // 1364 种单字符替换，预期漏过约 1.3 个
        assert_eq!(tried, 44 * 31);
        assert!(accepted <= 5, "漏过 {accepted} 个");
    }

    #[test]
    fn owner_tag_changes_room() {
        let a = sample();
        let b = RoomCode {
            owner_tag: [0x23; 10],
            ..a
        };
        assert_ne!(a.keys().1, b.keys().1);
    }

    const GOLDEN_CODE: &str = "GT2-248H-248H-248H-248H-248H-248H-24H2-48H2-48H2-48H2-4AZW";

    // ---------- GT3 ----------

    const OWNER: MemberId = [0x11; 32];
    const SALT: [u8; 16] = [0x22; 16];
    /// 参考值由 node:crypto（独立实现）算出：房主公钥 32 个 0x11、salt 16 个 0x22。
    const GOLDEN_COMMIT: [u8; 12] = [
        0x87, 0x12, 0xae, 0x9d, 0x20, 0xa5, 0xc4, 0x5e, 0x1a, 0x78, 0xaa, 0xc6,
    ];
    const GOLDEN_SHORT: &str = "GT3-GW9A-X790-MQ25-W6KR-NB3P";
    const GOLDEN_OWNER_CHECK: &str =
        "38db60d2de813fdacba7352459ee922ea2cb14f803b635f323dfa1c0fe6ba368";
    const GOLDEN_ROOM_KEY: &str =
        "5f1c494a5130e6638392c96da2fe4861a3d3836ef15e534d95ecc4caf45197b5";
    const GOLDEN_ROOM_ID: &str = "f23feb012b3978900780d9614e903ef1";

    fn short() -> ShortCode {
        ShortCode::new(&OWNER, &SALT)
    }

    #[test]
    fn short_code_matches_the_independent_reference() {
        let code = short();
        assert_eq!(code.commit, GOLDEN_COMMIT);
        assert_eq!(code.commit[11] & 1, 0, "只用前 95 位");
        let s = code.format();
        assert_eq!(s, GOLDEN_SHORT);
        assert_eq!(s.chars().filter(|c| *c != '-').count(), 3 + 20);
        assert_eq!(
            crypto::hex(&owner_check_of(&code.commit)),
            GOLDEN_OWNER_CHECK
        );
        let (key, id) = code.keys();
        assert_eq!(crypto::hex(&key), GOLDEN_ROOM_KEY);
        assert_eq!(crypto::hex(&id), GOLDEN_ROOM_ID);
    }

    #[test]
    fn team_code_parses_both_kinds_and_ignores_case_hyphens_spaces() {
        let s = short().format();
        assert_eq!(TeamCode::parse(&s), Ok(TeamCode::V3(short())));
        let messy = format!("  {} ", s.to_lowercase().replace('-', " "));
        assert_eq!(TeamCode::parse(&messy), Ok(TeamCode::V3(short())));
        assert_eq!(TeamCode::parse(&s).unwrap().format(), s);
        // 老的 GT2 码照旧
        let old = sample().format();
        assert_eq!(TeamCode::parse(&old), Ok(TeamCode::V2(sample())));
        assert_eq!(TeamCode::parse(&old).unwrap().format(), old);
        assert_eq!(TeamCode::parse(&old).unwrap().keys(), sample().keys());
    }

    #[test]
    fn short_code_rejects_prefix_length_and_check() {
        let s = short().format();
        assert_eq!(
            TeamCode::parse(&s.replacen("GT3", "GT4", 1)),
            Err(RoomCodeError::BadPrefix)
        );
        assert_eq!(
            TeamCode::parse(&s[..s.len() - 1]),
            Err(RoomCodeError::BadEncoding)
        );
        assert_eq!(
            TeamCode::parse(&format!("{s}0")),
            Err(RoomCodeError::BadEncoding)
        );
        // GT3 的前缀配 GT2 的长度、GT2 的前缀配 GT3 的长度，都不认
        let old = sample().format();
        assert_eq!(
            TeamCode::parse(&old.replacen("GT2", "GT3", 1)),
            Err(RoomCodeError::BadEncoding)
        );
        assert_eq!(
            TeamCode::parse(&s.replacen("GT3", "GT2", 1)),
            Err(RoomCodeError::BadEncoding)
        );
        let mut chars: Vec<char> = s.chars().collect();
        chars[10] = if chars[10] == 'A' { 'B' } else { 'A' };
        let typo: String = chars.into_iter().collect();
        assert_eq!(TeamCode::parse(&typo), Err(RoomCodeError::BadCheck));
    }

    #[test]
    fn short_code_single_char_typos_mostly_rejected() {
        let s = short().format();
        let alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
        let (mut tried, mut accepted) = (0, 0);
        for (i, orig) in s.char_indices().skip(4).filter(|(_, c)| *c != '-') {
            for c in alphabet.chars().filter(|c| *c != orig) {
                let mut t = s.clone();
                t.replace_range(i..i + 1, &c.to_string());
                tried += 1;
                if TeamCode::parse(&t).is_ok() {
                    accepted += 1;
                }
            }
        }
        // 620 种单字符替换，5 位校验预期漏过约 19 个（1/32）
        assert_eq!(tried, 20 * 31);
        assert!(accepted <= 40, "漏过 {accepted} 个");
    }

    #[test]
    fn owner_rule_admits_only_the_owner_with_the_right_salt() {
        let rule = TeamCode::V3(short()).owner_rule();
        assert!(rule.admits(&OWNER, Some(&SALT)));
        assert!(!rule.admits(&OWNER, None), "GT3 里没带 salt 的不算");
        assert!(!rule.admits(&OWNER, Some(&[0x23; 16])));
        // 队里的人知道 C 和房主的 salt，换成自己的公钥不行
        assert!(!rule.admits(&[0x12; 32], Some(&SALT)));
        // 没有建队事件时只认推导时记下的房主
        assert!(rule.is_owner(&OWNER, Some(&OWNER)));
        assert!(!rule.is_owner(&OWNER, None));
        assert!(!rule.is_owner(&[0x12; 32], Some(&OWNER)));

        // GT2 的规则不看 salt
        let old = TeamCode::V2(RoomCode {
            room_secret: [1; 16],
            owner_tag: owner_tag_of(&OWNER),
        })
        .owner_rule();
        assert!(old.admits(&OWNER, None));
        assert!(old.admits(&OWNER, Some(&SALT)));
        assert!(!old.admits(&[0x12; 32], None));
        assert!(old.is_owner(&OWNER, None));
    }

    #[test]
    fn different_salt_or_owner_gives_a_different_team() {
        let a = short();
        let b = ShortCode::new(&OWNER, &[0x23; 16]);
        let c = ShortCode::new(&[0x12; 32], &SALT);
        assert_ne!(a.keys().1, b.keys().1);
        assert_ne!(a.keys().1, c.keys().1);
        assert_ne!(a.format(), b.format());
    }

    #[test]
    fn codes_and_owner_rules_survive_local_storage() {
        for code in [TeamCode::V2(sample()), TeamCode::V3(short())] {
            assert_eq!(TeamCode::from_bytes(&code.to_bytes()), Some(code));
            let rule = code.owner_rule();
            assert_eq!(OwnerRule::from_bytes(rule.as_bytes()), Some(rule));
        }
        assert_eq!(TeamCode::V2(sample()).to_bytes().len(), 26);
        assert_eq!(TeamCode::V3(short()).to_bytes().len(), 12);
        for len in [0, 11, 13, 25, 27, 32] {
            assert_eq!(TeamCode::from_bytes(&vec![0; len]), None, "{len} 字节");
        }
        for len in [0, 9, 11, 31, 33] {
            assert_eq!(OwnerRule::from_bytes(&vec![0; len]), None, "{len} 字节");
        }
    }
}
