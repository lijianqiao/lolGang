//! 由状态事件推导队伍状态。
//!
//! 实现流程：
//! 1. 同一作者同一 seq 有多条（分叉）时，只取 event_id 最小的一条。
//! 2. 队长：有能解码的 RoomCreate、而且被队伍码的规则承认的作者（`room_code::OwnerRule`）：GT2 队伍是公钥摘要等于 owner_tag，
//!    GT3 队伍（3.3.0）是「公钥 + 建队事件里的 salt」重算出的队伍码主体对得上；取其 seq 最小的 RoomCreate。
//!    其他人发的 RoomCreate、Rename、Dissolve 一律不生效（队长信任根）。
//! 3. 成员：每位作者 seq 最大的 Join / Leave 决定是否在队；昵称和头像取 seq 最大的 Profile
//!    （这条 Profile 没带头像或头像不合格，就用自动生成的头像）。
//! 4. 队名：队长 seq 最大的 RoomCreate / Rename。
//! 5. 解散：队长 seq 最小的 Dissolve，其 ts_ms 就是 dissolve_ts。
//!
//! 整个推导是事件集合的纯函数，到达顺序不影响结果。

use std::collections::{BTreeMap, BTreeSet};

use crate::crypto::MemberId;
use crate::protocol::body::{self, Avatar, ProfileBody, RenameBody, RoomCreateBody};
use crate::protocol::event::{SignedEvent, kind};
use crate::protocol::room_code::OwnerRule;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoomState {
    pub owner: Option<MemberId>,
    pub title: Option<String>,
    pub members: BTreeSet<MemberId>,
    pub nicknames: BTreeMap<MemberId, String>,
    /// 只含最新 Profile 带了合格头像的人。
    pub avatars: BTreeMap<MemberId, Avatar>,
    pub dissolve_ts: Option<u64>,
}

/// 同一作者同一 seq 只留 event_id 最小的一条，按 (作者, seq) 升序。
pub fn dedup_forks(events: &[SignedEvent]) -> BTreeMap<(MemberId, u64), &SignedEvent> {
    let mut by_slot: BTreeMap<(MemberId, u64), &SignedEvent> = BTreeMap::new();
    for e in events {
        let slot = (e.event.author_id, e.event.seq);
        match by_slot.get(&slot) {
            Some(cur) if cur.event_id <= e.event_id => {}
            _ => {
                by_slot.insert(slot, e);
            }
        }
    }
    by_slot
}

/// GT2 队伍：房主是公钥摘要等于 `owner_tag` 的人。
pub fn derive_room_state(owner_tag: &[u8; 10], events: &[SignedEvent]) -> RoomState {
    derive_room_state_with(&OwnerRule::Tag(*owner_tag), events)
}

/// 两种队伍都用这个：认房主的规则由队伍码定（GT2 看公钥摘要，GT3 看建队事件里的 salt，见 `room_code::OwnerRule`）。
pub fn derive_room_state_with(rule: &OwnerRule, events: &[SignedEvent]) -> RoomState {
    let by_slot = dedup_forks(events);
    let mut st = RoomState::default();
    let mut last_membership: BTreeMap<MemberId, bool> = BTreeMap::new();

    // BTreeMap 按 (作者, seq) 升序遍历，同一作者后面的事件覆盖前面的
    for ((author, _seq), e) in &by_slot {
        let ev = &e.event;
        match ev.kind {
            kind::JOIN => {
                last_membership.insert(*author, true);
            }
            kind::LEAVE => {
                last_membership.insert(*author, false);
            }
            kind::PROFILE => {
                if let Ok(p) = body::decode::<ProfileBody>(&ev.body) {
                    st.nicknames.insert(*author, p.nickname);
                    match p.avatar.filter(Avatar::is_valid) {
                        Some(a) => st.avatars.insert(*author, a),
                        None => st.avatars.remove(author),
                    };
                }
            }
            kind::ROOM_CREATE => {
                if let Ok(c) = body::decode::<RoomCreateBody>(&ev.body)
                    && rule.admits(author, c.salt.as_ref())
                {
                    if st.owner.is_none() {
                        st.owner = Some(*author);
                    }
                    st.title = Some(c.title);
                }
            }
            kind::RENAME if st.owner == Some(*author) => {
                if let Ok(r) = body::decode::<RenameBody>(&ev.body) {
                    st.title = Some(r.title);
                }
            }
            kind::DISSOLVE if st.owner == Some(*author) && st.dissolve_ts.is_none() => {
                st.dissolve_ts = Some(ev.ts_ms);
            }
            _ => {} // 非队长的管理事件、命中、未知 kind：不影响队伍状态
        }
    }

    st.members = last_membership
        .into_iter()
        .filter(|(_, inside)| *inside)
        .map(|(m, _)| m)
        .collect();
    st
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Identity;
    use crate::protocol::body::encode;
    use crate::protocol::event::build_event;
    use crate::protocol::room_code::owner_tag_of;

    const ROOM: [u8; 16] = [1u8; 16];

    fn person(n: u8) -> Identity {
        Identity::from_secret(&[n; 32])
    }

    fn state(me: &Identity, seq: u64, ts: u64, k: u16, body: Vec<u8>) -> SignedEvent {
        build_event(me, ROOM, seq, ts, k, body)
    }

    fn create(me: &Identity, seq: u64, title: &str) -> SignedEvent {
        state(
            me,
            seq,
            seq * 1000,
            kind::ROOM_CREATE,
            encode(&RoomCreateBody {
                title: title.into(),
                salt: None,
            }),
        )
    }

    /// GT3 队伍的建队事件：带 salt。
    fn create_with_salt(me: &Identity, seq: u64, title: &str, salt: [u8; 16]) -> SignedEvent {
        state(
            me,
            seq,
            seq * 1000,
            kind::ROOM_CREATE,
            encode(&RoomCreateBody {
                title: title.into(),
                salt: Some(salt),
            }),
        )
    }

    /// GT3：只有「公钥 + salt」对得上队伍码的人才是房主；队里的人知道 salt 和队伍码也冒充不了。
    #[test]
    fn short_code_team_only_admits_the_real_owner() {
        use crate::protocol::room_code::{ShortCode, TeamCode};
        let owner = person(1);
        let mallory = person(2);
        let salt = [9u8; 16];
        let rule = TeamCode::V3(ShortCode::new(&owner.member_id(), &salt)).owner_rule();
        let events = vec![
            // 冒充的人排在前面（事件到达顺序不影响结果）：抄了房主的 salt、没带 salt、自己另取 salt，都不算
            create_with_salt(&mallory, 1, "抢来的", salt),
            create(&mallory, 2, "没带 salt"),
            create_with_salt(&mallory, 3, "自己的 salt", [8; 16]),
            state(&mallory, 4, 4000, kind::JOIN, vec![]),
            rename(&mallory, 5, 5000, "乱改"),
            state(&mallory, 6, 6000, kind::DISSOLVE, vec![]),
            create_with_salt(&owner, 1, "开黑", salt),
            state(&owner, 2, 2000, kind::JOIN, vec![]),
        ];
        let st = derive_room_state_with(&rule, &events);
        assert_eq!(st.owner, Some(owner.member_id()));
        assert_eq!(st.title.as_deref(), Some("开黑"));
        assert_eq!(st.dissolve_ts, None, "不是房主，解散不生效");
        assert_eq!(st.members.len(), 2);

        // 房主自己发的建队事件没带 salt 也不算（GT3 里认房主全靠 salt）
        let st = derive_room_state_with(&rule, &[create(&owner, 1, "开黑")]);
        assert_eq!(st.owner, None);

        // 房主的改名、解散照常生效
        let mut events = events;
        events.push(rename(&owner, 3, 3000, "五黑"));
        events.push(state(&owner, 4, 4500, kind::DISSOLVE, vec![]));
        let st = derive_room_state_with(&rule, &events);
        assert_eq!(st.title.as_deref(), Some("五黑"));
        assert_eq!(st.dissolve_ts, Some(4500));
    }

    /// GT2 队伍的规则没变：建队事件带不带 salt 都只看公钥摘要。
    #[test]
    fn long_code_team_ignores_salt() {
        let owner = person(1);
        let tag = owner_tag_of(&owner.member_id());
        let with = derive_room_state(&tag, &[create_with_salt(&owner, 1, "开黑", [3; 16])]);
        let without = derive_room_state(&tag, &[create(&owner, 1, "开黑")]);
        assert_eq!(with.owner, Some(owner.member_id()));
        assert_eq!(with, without);
    }

    fn rename(me: &Identity, seq: u64, ts: u64, title: &str) -> SignedEvent {
        state(
            me,
            seq,
            ts,
            kind::RENAME,
            encode(&RenameBody {
                title: title.into(),
            }),
        )
    }

    #[test]
    fn owner_members_and_title() {
        let owner = person(1);
        let bob = person(2);
        let tag = owner_tag_of(&owner.member_id());
        let events = vec![
            create(&owner, 1, "开黑"),
            state(&owner, 2, 2000, kind::JOIN, vec![]),
            state(
                &owner,
                3,
                3000,
                kind::PROFILE,
                encode(&ProfileBody {
                    nickname: "队长".into(),
                    avatar: None,
                }),
            ),
            state(&bob, 1, 1500, kind::JOIN, vec![]),
            rename(&owner, 4, 4000, "开黑 2"),
        ];
        let st = derive_room_state(&tag, &events);
        assert_eq!(st.owner, Some(owner.member_id()));
        assert_eq!(st.title.as_deref(), Some("开黑 2"));
        assert_eq!(
            st.members,
            [owner.member_id(), bob.member_id()].into_iter().collect()
        );
        assert_eq!(
            st.nicknames.get(&owner.member_id()).map(String::as_str),
            Some("队长")
        );
        assert_eq!(st.dissolve_ts, None);
    }

    #[test]
    fn non_owner_cannot_act_as_owner() {
        let owner = person(1);
        let mallory = person(3);
        let tag = owner_tag_of(&owner.member_id());
        let events = vec![
            create(&owner, 1, "开黑"),
            create(&mallory, 1, "被劫持"),
            rename(&mallory, 2, 2000, "被改名"),
            state(&mallory, 4, 4000, kind::DISSOLVE, vec![]),
        ];
        let st = derive_room_state(&tag, &events);
        assert_eq!(st.owner, Some(owner.member_id()));
        assert_eq!(st.title.as_deref(), Some("开黑"));
        assert_eq!(st.dissolve_ts, None);
    }

    #[test]
    fn leave_dissolve_and_order_independence() {
        let owner = person(1);
        let bob = person(2);
        let tag = owner_tag_of(&owner.member_id());
        let mut events = vec![
            create(&owner, 1, "开黑"),
            state(&owner, 2, 2000, kind::JOIN, vec![]),
            state(&bob, 1, 1500, kind::JOIN, vec![]),
            state(&bob, 2, 2500, kind::LEAVE, vec![]),
            state(&owner, 3, 50_000, kind::DISSOLVE, vec![]),
            state(&owner, 4, 60_000, kind::DISSOLVE, vec![]),
        ];
        let st = derive_room_state(&tag, &events);
        assert_eq!(st.members, [owner.member_id()].into_iter().collect());
        assert_eq!(st.dissolve_ts, Some(50_000));
        events.reverse();
        assert_eq!(derive_room_state(&tag, &events), st);
    }

    #[test]
    fn fork_takes_smallest_event_id() {
        let owner = person(1);
        let tag = owner_tag_of(&owner.member_id());
        let a = rename(&owner, 2, 2000, "甲");
        let b = rename(&owner, 2, 2001, "乙");
        let expected = if a.event_id < b.event_id {
            "甲"
        } else {
            "乙"
        };
        let st = derive_room_state(&tag, &[create(&owner, 1, "开黑"), a, b]);
        assert_eq!(st.title.as_deref(), Some(expected));
    }

    #[test]
    fn latest_profile_decides_avatar() {
        let owner = person(1);
        let tag = owner_tag_of(&owner.member_id());
        let profile = |seq: u64, avatar: Option<Avatar>| {
            state(
                &owner,
                seq,
                seq * 1000,
                kind::PROFILE,
                encode(&ProfileBody {
                    nickname: format!("第{seq}版"),
                    avatar,
                }),
            )
        };
        let preset = Avatar::Preset { icon: 1, color: 2 };
        let me = owner.member_id();
        let st = derive_room_state(&tag, &[profile(1, Some(preset.clone()))]);
        assert_eq!(st.avatars.get(&me), Some(&preset));
        // 后一条没带头像：恢复自动生成的头像，昵称照常更新
        let st = derive_room_state(&tag, &[profile(1, Some(preset.clone())), profile(2, None)]);
        assert_eq!(st.avatars.get(&me), None);
        assert_eq!(st.nicknames.get(&me).map(String::as_str), Some("第2版"));
        // 不合格的头像当作没有
        let bad = Avatar::Image(b"<svg/>".to_vec());
        let st = derive_room_state(&tag, &[profile(1, Some(preset)), profile(2, Some(bad))]);
        assert_eq!(st.avatars.get(&me), None);
    }
}
