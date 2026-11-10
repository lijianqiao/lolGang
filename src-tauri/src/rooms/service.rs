//! 建队、进队和队伍缓存（内部沿用 1.x 的「房间」叫法）。
//!
//! 实现流程：
//! 1. 建队（3.3.0 起用短队伍码 GT3，见 `protocol/room_code.rs`）：生成 16 字节随机 salt，和本人公钥算出队伍码的主体，
//!    派生出 room_id；队伍码用本机主密钥加密后存库，salt 写进 RoomCreate。
//!    在调用方的事务里写入 RoomCreate、Join，有昵称时再写 Profile，队伍直接是 Active。
//!    3.2.0 及以前建的队伍是长队伍码 GT2（room_secret 加房主公钥摘要），照常能进、能用，只是不再新建。
//! 2. 进队：解析队伍码（校验位不对立即报错），派生 room_id，记一条 Joining 状态的队伍。
//!    Joining 转 Active 要先和同伴同步状态事件（节点负责）。
//! 3. 缓存：状态事件变化后，用 derive_room_state 重新推导队名、队长、成员、昵称和解散时间，写回缓存表；
//!    推导出解散时间时，队伍转为 Dissolved。缓存都能从事件重建，权威始终是推导结果。

use crate::crypto::{self, Identity, MemberId, RoomId};
use crate::error::{AppError, AppResult};
use crate::keystore::{PURPOSE_ROOM_CODE, decrypt_local, encrypt_local};
use crate::protocol::body::{self, Avatar, ProfileBody, RoomCreateBody, TITLE_MAX_CHARS};
use crate::protocol::event::{SignedEvent, kind};
use crate::protocol::room_code::{ShortCode, TeamCode};
use crate::rooms::state::{RoomState, derive_room_state_with};
use crate::storage::Connection;
use crate::storage::events;
use crate::storage::rooms::{self as room_store, RoomRow, RoomStatus};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomSummary {
    pub room_id: RoomId,
    pub title: Option<String>,
    pub status: RoomStatus,
    pub is_owner: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberView {
    pub member_id: MemberId,
    pub fingerprint: String,
    pub nickname: Option<String>,
    pub is_owner: bool,
    pub in_room: bool,
    pub avatar: Option<Avatar>,
}

/// 这个队伍的队伍码（库里是用本机主密钥加密存的）。
fn stored_code(master: &[u8; 32], row: &RoomRow) -> AppResult<TeamCode> {
    TeamCode::from_bytes(&decrypt_local(master, PURPOSE_ROOM_CODE, &row.code_cipher)?)
        .ok_or(AppError::Decrypt)
}

pub fn get(conn: &Connection, room: &RoomId) -> AppResult<RoomRow> {
    room_store::get_room(conn, room)?.ok_or(AppError::RoomNotFound)
}

/// 返回 (room_id, 队伍码, 新写的事件)。
pub fn create_room(
    conn: &Connection,
    me: &Identity,
    master: &[u8; 32],
    profile: &ProfileBody,
    title: &str,
    now_ms: u64,
) -> AppResult<(RoomId, String, Vec<SignedEvent>)> {
    // 新建的队伍用短队伍码 GT3：salt 进建队事件，队友靠它认房主
    let salt: [u8; 16] = crypto::random_bytes()?;
    let code = TeamCode::V3(ShortCode::new(&me.member_id(), &salt));
    create_with(conn, me, master, profile, title, now_ms, code, Some(salt))
}

/// 测试用：照 3.2.0 及以前的做法建一个 GT2 队伍（长队伍码、建队事件不带 salt），用来测老队伍在新版本里照常工作。
#[cfg(test)]
pub fn create_room_v2(
    conn: &Connection,
    me: &Identity,
    master: &[u8; 32],
    profile: &ProfileBody,
    title: &str,
    now_ms: u64,
) -> AppResult<(RoomId, String, Vec<SignedEvent>)> {
    let code = TeamCode::V2(crate::protocol::room_code::RoomCode {
        room_secret: crypto::random_bytes()?,
        owner_tag: crate::protocol::room_code::owner_tag_of(&me.member_id()),
    });
    create_with(conn, me, master, profile, title, now_ms, code, None)
}

#[allow(clippy::too_many_arguments)]
fn create_with(
    conn: &Connection,
    me: &Identity,
    master: &[u8; 32],
    profile: &ProfileBody,
    title: &str,
    now_ms: u64,
    code: TeamCode,
    salt: Option<[u8; 16]>,
) -> AppResult<(RoomId, String, Vec<SignedEvent>)> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > TITLE_MAX_CHARS {
        return Err(AppError::InvalidTitle);
    }
    let (_, room) = code.keys();
    let cipher = encrypt_local(master, PURPOSE_ROOM_CODE, &code.to_bytes())?;
    room_store::insert_room(
        conn,
        &room,
        &cipher,
        &code.owner_rule(),
        RoomStatus::Active,
        now_ms,
    )?;

    let create = body::encode(&RoomCreateBody {
        title: title.to_string(),
        salt,
    });
    let mut written = vec![
        events::append_own(conn, me, &room, kind::ROOM_CREATE, create, now_ms)?,
        events::append_own(conn, me, &room, kind::JOIN, vec![], now_ms)?,
    ];
    if profile.has_content() {
        written.push(events::append_own(
            conn,
            me,
            &room,
            kind::PROFILE,
            body::encode(profile),
            now_ms,
        )?);
    }
    apply_state(conn, &room)?;
    Ok((room, code.format(), written))
}

/// 已经在这个队伍里就不重复建记录；之前退出过的（Left）重新进入 Joining。
pub fn join_room(
    conn: &Connection,
    master: &[u8; 32],
    code_str: &str,
    now_ms: u64,
) -> AppResult<RoomId> {
    let code = TeamCode::parse(code_str)?;
    let (_, room) = code.keys();
    let cipher = encrypt_local(master, PURPOSE_ROOM_CODE, &code.to_bytes())?;
    if !room_store::insert_room(
        conn,
        &room,
        &cipher,
        &code.owner_rule(),
        RoomStatus::Joining,
        now_ms,
    )? && get(conn, &room)?.status == RoomStatus::Left
    {
        room_store::set_status(conn, &room, RoomStatus::Joining)?;
    }
    Ok(room)
}

pub fn room_code(conn: &Connection, master: &[u8; 32], room: &RoomId) -> AppResult<String> {
    Ok(stored_code(master, &get(conn, room)?)?.format())
}

/// 队伍的全部事件。
pub fn all_events(conn: &Connection, room: &RoomId) -> AppResult<Vec<SignedEvent>> {
    Ok(events::query_room(conn, room)?
        .into_iter()
        .map(|s| s.event)
        .collect())
}

/// 从全部事件重新推导队伍状态，并写回缓存表。
pub fn apply_state(conn: &Connection, room: &RoomId) -> AppResult<RoomState> {
    let row = get(conn, room)?;
    let st = derive_room_state_with(&row.owner, &all_events(conn, room)?);
    room_store::update_cache(
        conn,
        room,
        st.title.as_deref(),
        st.owner.as_ref(),
        st.dissolve_ts,
    )?;
    let avatars = st
        .avatars
        .iter()
        .map(|(m, a)| (*m, body::encode(a)))
        .collect();
    room_store::set_members(conn, room, &st.members, &st.nicknames, &avatars)?;
    if st.dissolve_ts.is_some() && matches!(row.status, RoomStatus::Joining | RoomStatus::Active) {
        room_store::set_status(conn, room, RoomStatus::Dissolved)?;
    }
    Ok(st)
}

pub fn summary(me: &MemberId, row: &RoomRow) -> RoomSummary {
    RoomSummary {
        room_id: row.room_id,
        title: row.title.clone(),
        status: row.status,
        is_owner: row.owner.is_owner(me, row.owner_id.as_ref()),
    }
}

/// 队伍里出现过的所有人（含已退出的）。
pub fn list_members(conn: &Connection, room: &RoomId) -> AppResult<Vec<MemberView>> {
    let row = get(conn, room)?;
    Ok(room_store::members(conn, room)?
        .into_iter()
        .map(|m| MemberView {
            fingerprint: crypto::fingerprint(&m.member_id),
            is_owner: row.owner.is_owner(&m.member_id, row.owner_id.as_ref()),
            member_id: m.member_id,
            nickname: m.nickname,
            in_room: m.in_room,
            avatar: m.avatar.and_then(|b| body::decode::<Avatar>(&b).ok()),
        })
        .collect())
}

/// 这个队伍的包加密密钥（由队伍码派生）。
pub fn room_key(
    conn: &Connection,
    master: &[u8; 32],
    room: &RoomId,
) -> AppResult<crate::crypto::RoomKey> {
    Ok(stored_code(master, &get(conn, room)?)?.keys().0)
}
