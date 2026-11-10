//! 本机核心（Rust 标准库已有 `core`，为避免重名叫 `app_core`）：身份、队伍、状态事件。
//!
//! 实现流程：
//! 1. 启动：从凭据管理器取本机主密钥，读出或生成本机身份。
//! 2. 每个对外操作开一个数据库事务，调用各业务模块，全部成功才提交；返回本次新写的事件（交给节点发出去）。
//! 3. 同一时间只在一个队伍里（2.0 架构第 4 节）：建队、进队前先查有没有加入中或已加入的队伍。
//! 4. 入站（`ingest_events`）：无状态校验不过的丢弃；入库（重复跳过、分叉提示）；有新事件就重新推导队伍缓存。
//! 5. 实时帧：签名用本机身份（`sign_live`），收到的帧由节点验签，再用 `is_member` 查发送者是不是队员。
//! 6. 它是唯一知道所有模块的地方，业务模块之间不互相回调。
//! 7. 持有情况（每个作者有哪些序号）节点每 100 毫秒要用好几次，按「这个队伍最新一条事件的本地序号」缓存：
//!    序号没变就直接用，有新事件或事件被删才重新从库里算（3.1.1：以前每次都读这个队伍的全部事件）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::crypto::{EventId, Identity, MemberId, RoomId, RoomKey};
use crate::error::AppResult;
use crate::identity::{self, IdentityView};
use crate::keystore::KeyStore;
use crate::protocol::body::{self, Avatar, ProfileBody, RenameBody};
use crate::protocol::event::{SignedEvent, kind, verify_stateless};
use crate::protocol::frame::{HaveEntry, WantRange, short_id};
use crate::protocol::live::{LiveBody, WireLive, sign_live};
use crate::rooms::service as rooms;
use crate::storage::events::{self, PutResult};
use crate::storage::{Connection, rooms as room_store};
use crate::sync::plan::Holdings;

pub use crate::error::AppError;
pub use crate::rooms::service::{MemberView, RoomSummary};
pub use crate::storage::events::Delivery;
pub use crate::storage::rooms::RoomStatus;

/// 给界面的通知。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreEvent {
    RoomUpdated {
        room_id: RoomId,
    },
    ForkDetected {
        room_id: RoomId,
        member_id: MemberId,
    },
}

pub struct CreatedRoom {
    pub summary: RoomSummary,
    pub code: String,
    pub events: Vec<SignedEvent>,
}

/// 队伍 → （算的时候最新一条事件的本地序号，持有情况）。见文件头第 7 条。
type HoldingsCache = HashMap<RoomId, (Option<i64>, Arc<Holdings>)>;

pub struct Core {
    conn: Connection,
    me: Identity,
    view: IdentityView,
    master: [u8; 32],
    holdings_cache: RefCell<HoldingsCache>,
}

impl Core {
    pub fn open(conn: Connection, keystore: &dyn KeyStore) -> AppResult<Self> {
        let master = keystore.master_key()?;
        let (me, view) = identity::bootstrap(&conn, &master)?;
        Ok(Self {
            conn,
            me,
            view,
            master,
            holdings_cache: RefCell::default(),
        })
    }

    pub fn identity(&self) -> &IdentityView {
        &self.view
    }

    /// 改昵称和头像，并在当前队伍写一条 Profile 事件。头像为 None 表示用自动生成的头像。
    pub fn set_profile(
        &mut self,
        nickname: &str,
        avatar: Option<Avatar>,
        now_ms: u64,
    ) -> AppResult<Vec<SignedEvent>> {
        let tx = self.conn.transaction()?;
        identity::set_profile(&tx, nickname, avatar.as_ref())?;
        let profile = body::encode(&ProfileBody {
            nickname: nickname.to_string(),
            avatar: avatar.clone(),
        });
        let mut written = Vec::new();
        for row in room_store::list_rooms(&tx)? {
            if row.status != RoomStatus::Active {
                continue;
            }
            written.push(events::append_own(
                &tx,
                &self.me,
                &row.room_id,
                kind::PROFILE,
                profile.clone(),
                now_ms,
            )?);
            rooms::apply_state(&tx, &row.room_id)?;
        }
        tx.commit()?;
        self.view.nickname = nickname.to_string();
        self.view.avatar = avatar;
        Ok(written)
    }

    fn my_profile(&self) -> ProfileBody {
        ProfileBody {
            nickname: self.view.nickname.clone(),
            avatar: self.view.avatar.clone(),
        }
    }

    /// 当前队伍：加入中或已加入的那一个。
    pub fn current_team(&self) -> AppResult<Option<RoomSummary>> {
        Ok(room_store::list_rooms(&self.conn)?
            .iter()
            .find(|r| matches!(r.status, RoomStatus::Joining | RoomStatus::Active))
            .map(|r| rooms::summary(&self.view.member_id, r)))
    }

    fn require_no_team(&self) -> AppResult<()> {
        if self.current_team()?.is_some() {
            return Err(AppError::AlreadyInTeam);
        }
        Ok(())
    }

    pub fn create_room(&mut self, title: &str, now_ms: u64) -> AppResult<CreatedRoom> {
        self.require_no_team()?;
        let profile = self.my_profile();
        let tx = self.conn.transaction()?;
        let (room, code, written) =
            rooms::create_room(&tx, &self.me, &self.master, &profile, title, now_ms)?;
        let summary = rooms::summary(&self.view.member_id, &rooms::get(&tx, &room)?);
        tx.commit()?;
        Ok(CreatedRoom {
            summary,
            code,
            events: written,
        })
    }

    /// 测试用：照 3.2.0 及以前的做法建一个长队伍码（GT2）的队伍。
    #[cfg(test)]
    pub fn create_room_v2(&mut self, title: &str, now_ms: u64) -> AppResult<CreatedRoom> {
        self.require_no_team()?;
        let profile = self.my_profile();
        let tx = self.conn.transaction()?;
        let (room, code, written) =
            rooms::create_room_v2(&tx, &self.me, &self.master, &profile, title, now_ms)?;
        let summary = rooms::summary(&self.view.member_id, &rooms::get(&tx, &room)?);
        tx.commit()?;
        Ok(CreatedRoom {
            summary,
            code,
            events: written,
        })
    }

    pub fn join_room(&mut self, code: &str, now_ms: u64) -> AppResult<RoomSummary> {
        self.require_no_team()?;
        let tx = self.conn.transaction()?;
        let room = rooms::join_room(&tx, &self.master, code, now_ms)?;
        let summary = rooms::summary(&self.view.member_id, &rooms::get(&tx, &room)?);
        tx.commit()?;
        Ok(summary)
    }

    pub fn summary(&self, room: &RoomId) -> AppResult<RoomSummary> {
        Ok(rooms::summary(
            &self.view.member_id,
            &rooms::get(&self.conn, room)?,
        ))
    }

    pub fn room_code(&self, room: &RoomId) -> AppResult<String> {
        rooms::room_code(&self.conn, &self.master, room)
    }

    pub fn list_members(&self, room: &RoomId) -> AppResult<Vec<MemberView>> {
        rooms::list_members(&self.conn, room)
    }

    /// 现在在队里的人（不含已退出的）。
    pub fn members_in(&self, room: &RoomId) -> AppResult<Vec<MemberId>> {
        Ok(room_store::members(&self.conn, room)?
            .into_iter()
            .filter(|m| m.in_room)
            .map(|m| m.member_id)
            .collect())
    }

    /// 队长改队名，写一条 Rename。
    pub fn rename_room(
        &mut self,
        room: &RoomId,
        title: &str,
        now_ms: u64,
    ) -> AppResult<Vec<SignedEvent>> {
        let title = title.trim();
        if title.is_empty() || title.chars().count() > body::TITLE_MAX_CHARS {
            return Err(AppError::InvalidTitle);
        }
        let tx = self.conn.transaction()?;
        require_active_owner(&tx, &self.view.member_id, room)?;
        let b = body::encode(&RenameBody {
            title: title.to_string(),
        });
        let e = events::append_own(&tx, &self.me, room, kind::RENAME, b, now_ms)?;
        rooms::apply_state(&tx, room)?;
        tx.commit()?;
        Ok(vec![e])
    }

    /// 退出队伍：写一条 Leave，状态改为 Left。用队伍码可以重新进队。
    pub fn leave_room(&mut self, room: &RoomId, now_ms: u64) -> AppResult<Vec<SignedEvent>> {
        let tx = self.conn.transaction()?;
        if rooms::get(&tx, room)?.status != RoomStatus::Active {
            return Err(AppError::RoomNotActive);
        }
        let e = events::append_own(&tx, &self.me, room, kind::LEAVE, vec![], now_ms)?;
        rooms::apply_state(&tx, room)?;
        room_store::set_status(&tx, room, RoomStatus::Left)?;
        tx.commit()?;
        Ok(vec![e])
    }

    /// 取消加入：从没在这个队伍写过事件的，删除本机记录；以前是队员、退出后又重新进队的，退回 Left。
    pub fn cancel_join(&mut self, room: &RoomId) -> AppResult<()> {
        let me = self.view.member_id;
        let tx = self.conn.transaction()?;
        if rooms::get(&tx, room)?.status != RoomStatus::Joining {
            return Err(AppError::RoomNotJoining);
        }
        if events::query_by_author(&tx, room, &me)?.is_empty() {
            room_store::delete_room(&tx, room)?;
        } else {
            room_store::set_status(&tx, room, RoomStatus::Left)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 队长解散队伍（不可撤销）。返回新写的解散事件。
    pub fn dissolve_room(&mut self, room: &RoomId, now_ms: u64) -> AppResult<Vec<SignedEvent>> {
        let tx = self.conn.transaction()?;
        require_active_owner(&tx, &self.view.member_id, room)?;
        let e = events::append_own(&tx, &self.me, room, kind::DISSOLVE, vec![], now_ms)?;
        rooms::apply_state(&tx, room)?;
        tx.commit()?;
        Ok(vec![e])
    }

    /// 队长最早的一条解散事件，用于告诉还不知道解散的同伴。
    pub fn dissolve_event(&self, room: &RoomId) -> AppResult<Option<SignedEvent>> {
        let Some(owner) = rooms::get(&self.conn, room)?.owner_id else {
            return Ok(None);
        };
        Ok(
            events::query_kind_by_author(&self.conn, room, &owner, kind::DISSOLVE)?
                .into_iter()
                .next()
                .map(|s| s.event),
        )
    }

    /// 处理收到的一批事件。
    pub fn ingest_events(
        &mut self,
        room: &RoomId,
        incoming: &[SignedEvent],
    ) -> AppResult<Vec<CoreEvent>> {
        let tx = self.conn.transaction()?;
        rooms::get(&tx, room)?;
        let mut notes = Vec::new();
        let mut changed = false;
        for e in incoming {
            if verify_stateless(e, room).is_err() {
                continue;
            }
            match events::put_event(&tx, e, None)? {
                PutResult::Duplicate => continue,
                PutResult::Fork(_) => notes.push(CoreEvent::ForkDetected {
                    room_id: *room,
                    member_id: e.event.author_id,
                }),
                PutResult::Inserted(_) => {}
            }
            changed = true;
        }
        if changed {
            rooms::apply_state(&tx, room)?;
            notes.push(CoreEvent::RoomUpdated { room_id: *room });
        }
        tx.commit()?;
        Ok(notes)
    }

    // ---- 以下供节点使用 ----

    pub fn room_key(&self, room: &RoomId) -> AppResult<RoomKey> {
        rooms::room_key(&self.conn, &self.master, room)
    }

    pub fn room_status(&self, room: &RoomId) -> AppResult<RoomStatus> {
        Ok(rooms::get(&self.conn, room)?.status)
    }

    /// 需要联网的队伍：除了已退出的都要。
    pub fn sync_rooms(&self) -> AppResult<Vec<(RoomId, RoomStatus)>> {
        Ok(room_store::list_rooms(&self.conn)?
            .into_iter()
            .filter(|r| r.status != RoomStatus::Left)
            .map(|r| (r.room_id, r.status))
            .collect())
    }

    pub fn holdings(&self, room: &RoomId) -> AppResult<Arc<Holdings>> {
        let mark = events::latest_local_seq(&self.conn, room)?;
        if let Some((seen, h)) = self.holdings_cache.borrow().get(room)
            && *seen == mark
        {
            return Ok(h.clone());
        }
        let mut h = Holdings::default();
        for (author, seq) in events::holdings_rows(&self.conn, room)? {
            h.add(short_id(&author), seq);
        }
        let h = Arc::new(h);
        self.holdings_cache
            .borrow_mut()
            .insert(*room, (mark, h.clone()));
        Ok(h)
    }

    pub fn have_vector(&self, room: &RoomId) -> AppResult<Vec<HaveEntry>> {
        Ok(self.holdings(room)?.vector())
    }

    /// 回应 Want：按短 ID 和序号区间取本机持有的事件。
    pub fn events_for(&self, room: &RoomId, ranges: &[WantRange]) -> AppResult<Vec<SignedEvent>> {
        let authors = events::room_authors(&self.conn, room)?;
        let mut out = Vec::new();
        for r in ranges {
            for a in authors.iter().filter(|a| short_id(a) == r.author) {
                let found = events::query_range(&self.conn, room, a, r.from_seq, r.to_seq)?;
                out.extend(found.into_iter().map(|s| s.event));
            }
        }
        Ok(out)
    }

    /// 已拿到能对上 owner_tag 的建队事件。
    pub fn owner_known(&self, room: &RoomId) -> AppResult<bool> {
        Ok(rooms::get(&self.conn, room)?.owner_id.is_some())
    }

    /// Joining 转 Active：写入本人的 Join（还不是队员时）和 Profile。不是 Joining 时什么都不做。返回新写的事件。
    pub fn activate(&mut self, room: &RoomId, now_ms: u64) -> AppResult<Vec<SignedEvent>> {
        let profile = self.my_profile();
        let tx = self.conn.transaction()?;
        if rooms::get(&tx, room)?.status != RoomStatus::Joining {
            return Ok(Vec::new());
        }
        room_store::set_status(&tx, room, RoomStatus::Active)?;
        let me = self.view.member_id;
        let already_in = room_store::members(&tx, room)?
            .iter()
            .any(|m| m.member_id == me && m.in_room);
        let mut written = Vec::new();
        if !already_in {
            written.push(events::append_own(
                &tx,
                &self.me,
                room,
                kind::JOIN,
                vec![],
                now_ms,
            )?);
        }
        if profile.has_content() {
            written.push(events::append_own(
                &tx,
                &self.me,
                room,
                kind::PROFILE,
                body::encode(&profile),
                now_ms,
            )?);
        }
        rooms::apply_state(&tx, room)?;
        tx.commit()?;
        Ok(written)
    }

    /// 同伴的 Have 覆盖到的本人事件标记为已发送，返回改了的事件 ID。
    pub fn mark_delivered(&mut self, room: &RoomId, peer: &[HaveEntry]) -> AppResult<Vec<EventId>> {
        let me = self.view.member_id;
        let Some(e) = peer.iter().find(|e| e.author == short_id(&me)) else {
            return Ok(Vec::new());
        };
        let tx = self.conn.transaction()?;
        let ids = events::mark_sent(&tx, room, &me, e.max)?;
        tx.commit()?;
        Ok(ids)
    }

    /// 本人还没被确认的事件（节点启动时排进重发计划）。
    pub fn pending_events(&self, room: &RoomId) -> AppResult<Vec<SignedEvent>> {
        Ok(events::pending_own(&self.conn, room, &self.view.member_id)?
            .into_iter()
            .map(|s| s.event)
            .collect())
    }

    pub fn room_events(&self, room: &RoomId) -> AppResult<Vec<SignedEvent>> {
        rooms::all_events(&self.conn, room)
    }

    /// 用本机身份签一帧实时帧。
    pub fn sign_live(&self, room: &RoomId, seq: u64, ts_ms: u64, body: &LiveBody) -> WireLive {
        sign_live(&self.me, room, seq, ts_ms, body)
    }
}

/// 只有队长、且队伍是 Active 时才能改名、解散。
fn require_active_owner(conn: &Connection, me: &MemberId, room: &RoomId) -> AppResult<()> {
    let row = rooms::get(conn, room)?;
    if !row.owner.is_owner(me, row.owner_id.as_ref()) {
        return Err(AppError::NotOwner);
    }
    if row.status != RoomStatus::Active {
        return Err(AppError::RoomNotActive);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
