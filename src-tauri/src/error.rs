//! 统一错误类型。业务函数都返回 `AppResult<T>`，控制面板按变体显示提示。
//! 提示文字按界面语言出（3.3.2，`crate::tr!`），所以 `Display` 是手写的，不用 thiserror 的 `#[error("…")]`。

use std::fmt;

use crate::crypto::CryptoError;
use crate::protocol::room_code::RoomCodeError;

#[derive(Debug)]
pub enum AppError {
    InvalidRoomCode(RoomCodeError),
    RoomNotFound,
    NotOwner,
    RoomNotActive,
    RoomNotJoining,
    AlreadyInTeam,
    InvalidTitle,
    NicknameTooLong,
    InvalidAvatar,
    InvalidRelay(String),
    KeyStore(String),
    Decrypt,
    Storage(rusqlite::Error),
    Crypto(CryptoError),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::InvalidRoomCode(e) => write!(
                f,
                "{}",
                crate::tr!(
                    format!("队伍码不对：{e}"),
                    format!("Invalid team code: {e}")
                )
            ),
            AppError::RoomNotFound => {
                f.write_str(crate::tr!("队伍不存在", "The team doesn't exist"))
            }
            AppError::NotOwner => f.write_str(crate::tr!(
                "只有队长能这样做",
                "Only the team leader can do this"
            )),
            AppError::RoomNotActive => f.write_str(crate::tr!(
                "队伍现在不能这样操作",
                "The team can't do that right now"
            )),
            AppError::RoomNotJoining => f.write_str(crate::tr!(
                "只有正在加入的队伍可以取消",
                "Only a team you're still joining can be cancelled"
            )),
            AppError::AlreadyInTeam => f.write_str(crate::tr!(
                "已经在一个队伍里了，先退出再建队或进队",
                "You're already in a team. Leave it before creating or joining another"
            )),
            AppError::InvalidTitle => f.write_str(crate::tr!(
                "队名为空或太长，最多 64 个字符",
                "Team name is empty or too long (64 characters at most)"
            )),
            AppError::NicknameTooLong => f.write_str(crate::tr!(
                "昵称太长，最多 32 个字符",
                "Nickname too long (32 characters at most)"
            )),
            AppError::InvalidAvatar => f.write_str(crate::tr!(
                "头像图片太大或格式不对，请换一张",
                "The avatar image is too large or in the wrong format. Please choose another"
            )),
            AppError::InvalidRelay(url) => write!(
                f,
                "{}",
                crate::tr!(
                    format!("中继地址必须是 wss:// 开头的完整地址：{url}"),
                    format!("A relay address must be a full address starting with wss://: {url}")
                )
            ),
            AppError::KeyStore(why) => write!(
                f,
                "{}",
                crate::tr!(
                    format!("本机密钥不可用：{why}"),
                    format!("The local key is unavailable: {why}")
                )
            ),
            AppError::Decrypt => f.write_str(crate::tr!(
                "本机数据解密失败，可能是本机主密钥被更换",
                "Couldn't decrypt local data; the local master key may have been replaced"
            )),
            AppError::Storage(e) => write!(
                f,
                "{}",
                crate::tr!(format!("数据库错误：{e}"), format!("Database error: {e}"))
            ),
            AppError::Crypto(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AppError::InvalidRoomCode(e) => Some(e),
            AppError::Storage(e) => Some(e),
            // 以前是 #[error(transparent)]：自己就显示里面那个错误，source 也是它的 source
            AppError::Crypto(e) => e.source(),
            _ => None,
        }
    }
}

impl From<RoomCodeError> for AppError {
    fn from(e: RoomCodeError) -> Self {
        AppError::InvalidRoomCode(e)
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Storage(e)
    }
}

impl From<CryptoError> for AppError {
    fn from(e: CryptoError) -> Self {
        AppError::Crypto(e)
    }
}

pub type AppResult<T> = Result<T, AppError>;
