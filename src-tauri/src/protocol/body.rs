//! 事件内容（body）。
//!
//! 实现流程：
//! 1. 每种事件的内容是一个结构体，用 CBOR 编码成字节放进事件。CBOR 是二进制版的 JSON，体积小、字段可扩展。
//! 2. body 只由作者编码一次；收端按原始字节验 ID，需要时再解码，解码失败就当作这条事件不生效。
//! 3. 新字段只能是可选的、追加在末尾：旧版本解码时忽略不认识的字段，新版本解码旧事件时缺的字段取默认值。
//! 4. 头像（`Avatar`）放在 Profile 里随昵称一起同步，不分片：内置头像只是两个小数字；
//!    上传的图片由界面压到 800 字节以内，只认 WebP、PNG、JPEG 的文件头。
//! 5. 2.0 去掉了 1.x 的聊天、图片、改规则和聊天叠层的内容；2.0 加的对线命中（Hit）3.0 又去掉了。

use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const NICKNAME_MAX_CHARS: usize = 32;
pub const TITLE_MAX_CHARS: usize = 64;
/// 上传头像图片的上限。加上 32 个字的昵称，整个 Profile 仍在 1024 字节以内。
pub const AVATAR_IMAGE_MAX_BYTES: usize = 800;
/// 内置头像的图标数和底色数。界面里的列表只能在末尾追加，已有的下标不能改。
pub const AVATAR_PRESET_ICONS: u16 = 48;
pub const AVATAR_PRESET_COLORS: u8 = 12;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomCreateBody {
    pub title: String,
    /// GT3 队伍码（3.3.0）的建队事件带着它：队友用「作者公钥 + salt」重算队伍码的主体来认房主（`room_code::OwnerRule`）。
    /// GT2 队伍的建队事件没有这一项，编码出来和以前一个字节不差。
    #[serde(default, skip_serializing_if = "Option::is_none", with = "serde_bytes")]
    pub salt: Option<[u8; 16]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameBody {
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileBody {
    pub nickname: String,
    /// 没有时用按指纹自动生成的头像。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<Avatar>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Avatar {
    /// 内置头像：第几个图标、第几种底色。
    Preset { icon: u16, color: u8 },
    /// 上传的图片。
    Image(#[serde(with = "serde_bytes")] Vec<u8>),
}

impl ProfileBody {
    /// 昵称和头像都没设置时不用写 Profile。
    pub fn has_content(&self) -> bool {
        !self.nickname.is_empty() || self.avatar.is_some()
    }
}

impl Avatar {
    /// 内置头像的下标在范围内；图片不超过上限、文件头是认识的格式。
    pub fn is_valid(&self) -> bool {
        match self {
            Avatar::Preset { icon, color } => {
                *icon < AVATAR_PRESET_ICONS && *color < AVATAR_PRESET_COLORS
            }
            Avatar::Image(b) => b.len() <= AVATAR_IMAGE_MAX_BYTES && image_mime(b).is_some(),
        }
    }
}

/// 按文件头认图片格式：WebP、PNG、JPEG，其余返回 None。
pub fn image_mime(b: &[u8]) -> Option<&'static str> {
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else if b.starts_with(b"\x89PNG") {
        Some("image/png")
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else {
        None
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("事件内容无法解码")]
pub struct BodyError;

pub fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    ciborium::into_writer(value, &mut out).expect("写入内存不会失败");
    out
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, BodyError> {
    ciborium::from_reader(bytes).map_err(|_| BodyError)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::event::MAX_BODY_LEN;

    #[test]
    fn roundtrip_all_bodies() {
        let create = RoomCreateBody {
            title: "开黑".into(),
            salt: None,
        };
        // 没有 salt 的建队事件（GT2 队伍）编码出来和 3.2.0 一个字节不差：事件 ID 是按这些字节算的
        assert_eq!(
            encode(&create),
            [
                0xa1, 0x65, b't', b'i', b't', b'l', b'e', 0x66, 0xe5, 0xbc, 0x80, 0xe9, 0xbb, 0x91
            ]
        );
        assert_eq!(decode::<RoomCreateBody>(&encode(&create)), Ok(create));
        // GT3 队伍的建队事件带 16 字节的 salt；长度不对的解不出来（这条事件就不生效）
        let with_salt = RoomCreateBody {
            title: "开黑".into(),
            salt: Some([7; 16]),
        };
        let bytes = encode(&with_salt);
        assert_eq!(decode::<RoomCreateBody>(&bytes), Ok(with_salt));
        let short_salt: Vec<u8> = bytes[..bytes.len() - 1].to_vec();
        let mut short_salt = short_salt;
        let at = short_salt.len() - 16;
        short_salt[at] = 0x4f; // 字节串的长度从 16 改成 15
        assert_eq!(decode::<RoomCreateBody>(&short_salt), Err(BodyError));
        let rename = RenameBody {
            title: "五黑".into(),
        };
        assert_eq!(decode::<RenameBody>(&encode(&rename)), Ok(rename));
    }

    #[test]
    fn garbage_does_not_decode() {
        assert_eq!(decode::<ProfileBody>(&[0xff, 0x00, 0x13]), Err(BodyError));
        assert_eq!(
            decode::<ProfileBody>(&encode(&RenameBody { title: "x".into() })),
            Err(BodyError)
        );
    }

    /// 事件内容（最多 1 KB）里藏着很深的嵌套或谎报的长度：解不出来，不爆栈、不按谎报的长度分配内存。
    #[test]
    fn deep_nesting_and_lying_lengths_do_not_decode() {
        // {"zz": <value>}：不认识的字段要整个跳过
        let in_unknown_field = |value: &[u8]| {
            let mut b = vec![0xa1, 0x62, b'z', b'z'];
            b.extend_from_slice(value);
            b
        };
        let mut deep = vec![0x81; MAX_BODY_LEN];
        deep.push(0x00);
        assert_eq!(
            decode::<ProfileBody>(&in_unknown_field(&deep)),
            Err(BodyError)
        );
        for head in [0x5b, 0x7b, 0x9b, 0xbb] {
            let mut lie = vec![head];
            lie.extend([0xff; 8]);
            assert_eq!(
                decode::<ProfileBody>(&in_unknown_field(&lie)),
                Err(BodyError)
            );
        }
    }

    fn webp(len: usize) -> Vec<u8> {
        let mut b = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        b.resize(len, 7);
        b
    }

    #[test]
    fn profile_without_avatar_omits_the_field() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct NameOnly {
            nickname: String,
        }
        let with = ProfileBody {
            nickname: "阿钢".into(),
            avatar: Some(Avatar::Image(webp(100))),
        };
        assert_eq!(decode::<ProfileBody>(&encode(&with)), Ok(with));
        let without = ProfileBody {
            nickname: "乙".into(),
            avatar: None,
        };
        assert_eq!(
            encode(&without),
            encode(&NameOnly {
                nickname: "乙".into()
            })
        );
        let preset = ProfileBody {
            nickname: String::new(),
            avatar: Some(Avatar::Preset { icon: 3, color: 7 }),
        };
        assert_eq!(decode::<ProfileBody>(&encode(&preset)), Ok(preset));
    }

    #[test]
    fn avatar_validity() {
        assert!(Avatar::Image(webp(AVATAR_IMAGE_MAX_BYTES)).is_valid());
        assert!(!Avatar::Image(webp(AVATAR_IMAGE_MAX_BYTES + 1)).is_valid());
        assert!(Avatar::Image(b"\x89PNG\r\n\x1a\n".to_vec()).is_valid());
        assert!(Avatar::Image(vec![0xff, 0xd8, 0xff, 0xe0]).is_valid());
        assert!(!Avatar::Image(b"<svg></svg>".to_vec()).is_valid());
        assert!(!Avatar::Image(vec![]).is_valid());
        assert!(
            Avatar::Preset {
                icon: AVATAR_PRESET_ICONS - 1,
                color: AVATAR_PRESET_COLORS - 1
            }
            .is_valid()
        );
        assert!(
            !Avatar::Preset {
                icon: AVATAR_PRESET_ICONS,
                color: 0
            }
            .is_valid()
        );
        assert!(
            !Avatar::Preset {
                icon: 0,
                color: AVATAR_PRESET_COLORS
            }
            .is_valid()
        );
        assert_eq!(image_mime(&webp(20)), Some("image/webp"));
    }

    #[test]
    fn largest_profile_fits() {
        // 32 个四字节字符的昵称加上最大的头像图片
        let profile = ProfileBody {
            nickname: "😀".repeat(NICKNAME_MAX_CHARS),
            avatar: Some(Avatar::Image(webp(AVATAR_IMAGE_MAX_BYTES))),
        };
        let n = encode(&profile).len();
        assert!(n <= MAX_BODY_LEN, "Profile body {n} 字节");
    }
}
