//! Core 的测试：建队、进队、只能在一个队伍里、资料、入站顺序无关、只有队长能改名和解散、
//! 篡改和重复、同步激活、退出和取消、送达确认、实时帧签名、重启保留。

use super::*;
use crate::keystore::MemoryKeyStore;
use crate::protocol::live::{LiveBody, open_live};
use crate::protocol::room_code::{RoomCode, TeamCode, owner_tag_of};
use crate::storage;
use crate::sync::plan::{ready_to_activate, state_gaps};

fn new_core() -> Core {
    Core::open(
        storage::open_in_memory().unwrap(),
        &MemoryKeyStore([1u8; 32]),
    )
    .unwrap()
}

/// 甲建队，乙用队伍码进队、补齐、激活，甲收到乙的进队事件。
fn pair() -> (Core, Core, RoomId) {
    let mut a = new_core();
    a.set_profile("队长", None, 1).unwrap();
    let created = a.create_room("开黑", 1000).unwrap();
    let room = created.summary.room_id;
    let mut b = new_core();
    b.set_profile("乙", None, 2).unwrap();
    b.join_room(&created.code, 2000).unwrap();
    b.ingest_events(&room, &a.room_events(&room).unwrap())
        .unwrap();
    let written = b.activate(&room, 3000).unwrap();
    a.ingest_events(&room, &written).unwrap();
    (a, b, room)
}

fn in_room(c: &Core, room: &RoomId, who: &MemberId) -> bool {
    c.members_in(room).unwrap().contains(who)
}

#[test]
fn create_room_makes_me_owner_and_member() {
    let mut a = new_core();
    a.set_profile("阿钢", None, 1).unwrap();
    let created = a.create_room("开黑", 1000).unwrap();
    let room = created.summary.room_id;
    assert!(created.summary.is_owner);
    assert_eq!(created.summary.status, RoomStatus::Active);
    assert_eq!(created.summary.title.as_deref(), Some("开黑"));
    assert_eq!(
        created
            .events
            .iter()
            .map(|e| e.event.kind)
            .collect::<Vec<_>>(),
        vec![kind::ROOM_CREATE, kind::JOIN, kind::PROFILE]
    );
    // 新建的队伍用短队伍码（3.3.0）：GT3 加 20 个字符
    assert!(created.code.starts_with("GT3-"));
    assert_eq!(created.code.chars().filter(|c| *c != '-').count(), 3 + 20);
    let code = TeamCode::parse(&created.code).unwrap();
    assert!(matches!(code, TeamCode::V3(_)));
    assert_eq!(code.keys().1, room);
    assert_eq!(a.room_code(&room).unwrap(), created.code);
    let members = a.list_members(&room).unwrap();
    assert_eq!(members.len(), 1);
    assert!(members[0].is_owner && members[0].in_room);
    assert_eq!(members[0].nickname.as_deref(), Some("阿钢"));
    assert_eq!(a.current_team().unwrap().map(|t| t.room_id), Some(room));
}

/// 3.2.0 及以前建的队伍（长队伍码 GT2）在新版本里照常：进得去、认得出房主、只有房主能改名和解散。
#[test]
fn long_code_teams_from_older_versions_still_work() {
    let mut a = new_core();
    a.set_profile("队长", None, 1).unwrap();
    let created = a.create_room_v2("老队伍", 1000).unwrap();
    let room = created.summary.room_id;
    assert!(created.code.starts_with("GT2-"));
    let code = RoomCode::parse(&created.code).unwrap();
    assert_eq!(code.owner_tag, owner_tag_of(&a.identity().member_id));
    assert_eq!(code.keys().1, room);
    assert_eq!(a.room_code(&room).unwrap(), created.code);
    assert!(created.summary.is_owner);

    let mut b = new_core();
    b.join_room(&created.code, 2000).unwrap();
    b.ingest_events(&room, &a.room_events(&room).unwrap())
        .unwrap();
    assert!(b.owner_known(&room).unwrap());
    let written = b.activate(&room, 3000).unwrap();
    a.ingest_events(&room, &written).unwrap();
    assert!(!b.summary(&room).unwrap().is_owner);
    assert!(matches!(
        b.rename_room(&room, "乱改", 4000),
        Err(AppError::NotOwner)
    ));
    let renamed = a.rename_room(&room, "五黑", 5000).unwrap();
    b.ingest_events(&room, &renamed).unwrap();
    assert_eq!(b.summary(&room).unwrap().title.as_deref(), Some("五黑"));
    let a_id = a.identity().member_id;
    let owners: Vec<MemberId> = b
        .list_members(&room)
        .unwrap()
        .into_iter()
        .filter(|m| m.is_owner)
        .map(|m| m.member_id)
        .collect();
    assert_eq!(owners, vec![a_id]);
}

/// 短队伍码（GT3）的队伍：进队的人靠建队事件里的 salt 认出房主；只有房主能改名和解散；
/// 队友自己写一条建队事件（抄房主的 salt 也好）不会变成房主。
#[test]
fn short_code_team_knows_its_owner_and_rejects_impostors() {
    let (mut a, mut b, room) = pair();
    let (a_id, b_id) = (a.identity().member_id, b.identity().member_id);
    assert!(a.summary(&room).unwrap().is_owner);
    assert!(!b.summary(&room).unwrap().is_owner);
    for core in [&a, &b] {
        let owners: Vec<MemberId> = core
            .list_members(&room)
            .unwrap()
            .into_iter()
            .filter(|m| m.is_owner)
            .map(|m| m.member_id)
            .collect();
        assert_eq!(owners, vec![a_id], "两边都认甲是房主");
    }
    assert!(matches!(
        b.rename_room(&room, "乱改", 4000),
        Err(AppError::NotOwner)
    ));
    assert!(matches!(
        b.dissolve_room(&room, 4000),
        Err(AppError::NotOwner)
    ));

    // 乙绕过界面，自己签一条建队事件，salt 抄房主的：两边都不认
    let salt = a
        .room_events(&room)
        .unwrap()
        .iter()
        .find(|e| e.event.kind == kind::ROOM_CREATE)
        .and_then(|e| body::decode::<body::RoomCreateBody>(&e.event.body).ok())
        .and_then(|c| c.salt)
        .expect("短队伍码的建队事件带 salt");
    let forged = events::append_own(
        &b.conn,
        &b.me,
        &room,
        kind::ROOM_CREATE,
        body::encode(&body::RoomCreateBody {
            title: "抢来的".into(),
            salt: Some(salt),
        }),
        5000,
    )
    .unwrap();
    rooms::apply_state(&b.conn, &room).unwrap();
    a.ingest_events(&room, &[forged]).unwrap();
    for core in [&a, &b] {
        let s = core.summary(&room).unwrap();
        assert_eq!(s.title.as_deref(), Some("开黑"));
        assert!(
            core.list_members(&room)
                .unwrap()
                .iter()
                .all(|m| m.is_owner == (m.member_id == a_id))
        );
    }
    assert!(!b.summary(&room).unwrap().is_owner);
    let _ = b_id;

    // 真房主的改名照常生效
    let renamed = a.rename_room(&room, "五黑", 6000).unwrap();
    b.ingest_events(&room, &renamed).unwrap();
    assert_eq!(b.summary(&room).unwrap().title.as_deref(), Some("五黑"));
}

#[test]
fn titles_are_checked() {
    let mut a = new_core();
    assert!(matches!(
        a.create_room("  ", 1),
        Err(AppError::InvalidTitle)
    ));
    assert!(matches!(
        a.create_room(&"字".repeat(65), 1),
        Err(AppError::InvalidTitle)
    ));
    assert!(a.current_team().unwrap().is_none());
}

#[test]
fn only_one_team_at_a_time() {
    let mut a = new_core();
    let created = a.create_room("开黑", 1000).unwrap();
    assert!(matches!(
        a.create_room("再来一个", 2000),
        Err(AppError::AlreadyInTeam)
    ));
    let mut other = new_core();
    let other_code = other.create_room("别的队", 1000).unwrap().code;
    assert!(matches!(
        a.join_room(&other_code, 2000),
        Err(AppError::AlreadyInTeam)
    ));
    // 退出以后就能进别的队
    a.leave_room(&created.summary.room_id, 3000).unwrap();
    assert!(a.current_team().unwrap().is_none());
    let joined = a.join_room(&other_code, 4000).unwrap();
    assert_eq!(joined.status, RoomStatus::Joining);
    assert_eq!(
        a.current_team().unwrap().map(|t| t.room_id),
        Some(joined.room_id)
    );
}

#[test]
fn join_parses_code_and_waits_for_sync() {
    let mut a = new_core();
    let created = a.create_room("开黑", 1000).unwrap();
    let mut b = new_core();
    assert!(matches!(
        b.join_room("GT2-坏码", 1),
        Err(AppError::InvalidRoomCode(_))
    ));
    let s = b.join_room(&created.code, 2000).unwrap();
    assert_eq!(s.room_id, created.summary.room_id);
    assert_eq!(s.status, RoomStatus::Joining);
    assert!(!s.is_owner);
    assert!(!b.owner_known(&s.room_id).unwrap());
}

#[test]
fn joining_member_catches_up_then_activates() {
    let mut a = new_core();
    a.set_profile("队长", None, 1).unwrap();
    let created = a.create_room("开黑", 1000).unwrap();
    let room = created.summary.room_id;
    let mut b = new_core();
    b.set_profile("新人", None, 2).unwrap();
    b.join_room(&created.code, 2000).unwrap();

    // 乙只看甲的 Have 算缺口，按区间向甲要，甲取出事件交给乙
    let a_vector = a.have_vector(&room).unwrap();
    let gaps = state_gaps(&b.holdings(&room).unwrap(), &a_vector);
    let served = a.events_for(&room, &gaps).unwrap();
    assert_eq!(served.len(), 3, "建队、进队、资料");
    b.ingest_events(&room, &served).unwrap();
    assert!(b.owner_known(&room).unwrap());
    assert!(ready_to_activate(
        true,
        &b.holdings(&room).unwrap(),
        &[a_vector]
    ));

    let written = b.activate(&room, 4000).unwrap();
    assert_eq!(
        written.iter().map(|e| e.event.kind).collect::<Vec<_>>(),
        vec![kind::JOIN, kind::PROFILE]
    );
    assert_eq!(b.summary(&room).unwrap().status, RoomStatus::Active);
    assert!(
        b.activate(&room, 5000).unwrap().is_empty(),
        "已经是 Active 时不再重复写"
    );

    a.ingest_events(&room, &written).unwrap();
    assert!(in_room(&a, &room, &b.identity().member_id));
    assert_eq!(a.members_in(&room).unwrap().len(), 2);
}

#[test]
fn profile_reaches_teammates_with_avatar() {
    let (mut a, mut b, room) = pair();
    let avatar = Avatar::Preset { icon: 5, color: 3 };
    let written = b.set_profile("乙乙", Some(avatar.clone()), 5000).unwrap();
    assert_eq!(written.len(), 1);
    a.ingest_events(&room, &written).unwrap();
    let bm = a
        .list_members(&room)
        .unwrap()
        .into_iter()
        .find(|m| m.member_id == b.identity().member_id)
        .unwrap();
    assert_eq!(
        (bm.nickname.as_deref(), bm.avatar),
        (Some("乙乙"), Some(avatar))
    );
    assert!(matches!(
        b.set_profile("太长".repeat(20).as_str(), None, 6000),
        Err(AppError::NicknameTooLong)
    ));
    assert!(matches!(
        b.set_profile("乙", Some(Avatar::Image(b"<svg/>".to_vec())), 6000),
        Err(AppError::InvalidAvatar)
    ));
}

#[test]
fn events_in_any_order_give_the_same_team() {
    let (a, b, room) = pair();
    let mut events = a.room_events(&room).unwrap();
    let mut c = new_core();
    c.join_room(&a.room_code(&room).unwrap(), 1).unwrap();
    events.reverse();
    c.ingest_events(&room, &events).unwrap();
    let names = |core: &Core| {
        core.list_members(&room)
            .unwrap()
            .into_iter()
            .map(|m| (m.member_id, m.nickname, m.in_room))
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&c), names(&a));
    assert_eq!(names(&b), names(&a));
}

#[test]
fn only_the_owner_renames_and_dissolves() {
    let (mut a, mut b, room) = pair();
    assert!(matches!(
        b.rename_room(&room, "乙的队", 5000),
        Err(AppError::NotOwner)
    ));
    assert!(matches!(
        b.dissolve_room(&room, 5000),
        Err(AppError::NotOwner)
    ));
    let e = a.rename_room(&room, "五黑", 5000).unwrap();
    b.ingest_events(&room, &e).unwrap();
    assert_eq!(b.summary(&room).unwrap().title.as_deref(), Some("五黑"));
    let e = a.dissolve_room(&room, 6000).unwrap();
    assert_eq!(a.summary(&room).unwrap().status, RoomStatus::Dissolved);
    b.ingest_events(&room, &e).unwrap();
    assert_eq!(b.summary(&room).unwrap().status, RoomStatus::Dissolved);
    assert_eq!(
        b.dissolve_event(&room).unwrap().map(|d| d.event_id),
        Some(e[0].event_id)
    );
    assert!(
        b.current_team().unwrap().is_none(),
        "解散的队伍不算当前队伍"
    );
}

#[test]
fn tampered_and_duplicate_events_are_ignored_forks_reported() {
    let (a, mut b, room) = pair();
    let events = a.room_events(&room).unwrap();
    assert!(
        b.ingest_events(&room, &events).unwrap().is_empty(),
        "全是重复的：没有变化"
    );
    let mut bad = events[0].clone();
    bad.sig[0] ^= 1;
    assert!(b.ingest_events(&room, &[bad]).unwrap().is_empty());
    // 同一作者同一序号的另一条事件：分叉
    let me = Identity::from_secret(&[9u8; 32]);
    let x = crate::protocol::event::build_event(&me, room, 1, 1, kind::JOIN, vec![]);
    let y = crate::protocol::event::build_event(&me, room, 1, 2, kind::LEAVE, vec![]);
    b.ingest_events(&room, &[x]).unwrap();
    let notes = b.ingest_events(&room, &[y]).unwrap();
    assert!(notes.contains(&CoreEvent::ForkDetected {
        room_id: room,
        member_id: me.member_id()
    }));
    assert!(matches!(
        b.ingest_events(&[9u8; 16], &events),
        Err(AppError::RoomNotFound)
    ));
}

#[test]
fn leaving_and_rejoining_restores_membership() {
    let (mut a, mut b, room) = pair();
    let me = b.identity().member_id;
    let left = b.leave_room(&room, 5000).unwrap();
    assert_eq!(b.summary(&room).unwrap().status, RoomStatus::Left);
    a.ingest_events(&room, &left).unwrap();
    assert!(!in_room(&a, &room, &me));
    b.join_room(&a.room_code(&room).unwrap(), 6000).unwrap();
    b.ingest_events(&room, &a.room_events(&room).unwrap())
        .unwrap();
    let back = b.activate(&room, 7000).unwrap();
    a.ingest_events(&room, &back).unwrap();
    assert!(in_room(&a, &room, &me));
}

#[test]
fn cancelling_a_fresh_join_deletes_the_team_and_a_rejoin_goes_back_to_left() {
    let (a, mut b, room) = pair();
    let mut c = new_core();
    c.join_room(&a.room_code(&room).unwrap(), 1).unwrap();
    c.cancel_join(&room).unwrap();
    assert!(matches!(c.summary(&room), Err(AppError::RoomNotFound)));

    b.leave_room(&room, 5000).unwrap();
    b.join_room(&a.room_code(&room).unwrap(), 6000).unwrap();
    b.cancel_join(&room).unwrap();
    assert_eq!(b.summary(&room).unwrap().status, RoomStatus::Left);
    assert!(matches!(
        b.cancel_join(&room),
        Err(AppError::RoomNotJoining)
    ));
}

#[test]
fn peer_have_confirms_my_events() {
    let (mut a, b, room) = pair();
    let mine = a.pending_events(&room).unwrap();
    assert_eq!(mine.len(), 3);
    let confirmed = a
        .mark_delivered(&room, &b.have_vector(&room).unwrap())
        .unwrap();
    assert_eq!(confirmed.len(), 3);
    assert!(a.pending_events(&room).unwrap().is_empty());
}

#[test]
fn room_key_matches_code_and_sync_rooms_skip_left() {
    let (a, mut b, room) = pair();
    let code = TeamCode::parse(&a.room_code(&room).unwrap()).unwrap();
    assert_eq!(a.room_key(&room).unwrap(), code.keys().0);
    assert_eq!(b.sync_rooms().unwrap(), vec![(room, RoomStatus::Active)]);
    b.leave_room(&room, 5000).unwrap();
    assert!(b.sync_rooms().unwrap().is_empty());
}

#[test]
fn live_frames_are_signed_with_my_identity() {
    let (a, _b, room) = pair();
    let w = a.sign_live(&room, 7, 1000, &LiveBody::Bye);
    let opened = open_live(&w, &room).unwrap();
    assert_eq!(opened.from, a.identity().member_id);
    assert_eq!(opened.body, LiveBody::Bye);
}

#[test]
fn everything_survives_reopen() {
    let tag = crate::crypto::random_bytes::<8>()
        .map(u64::from_le_bytes)
        .unwrap();
    let path = std::env::temp_dir().join(format!("gang2-restart-{tag}.db"));
    let keys = MemoryKeyStore([7u8; 32]);
    let (me, room, code) = {
        let mut a = Core::open(storage::open(&path).unwrap(), &keys).unwrap();
        a.set_profile("阿钢", None, 500).unwrap();
        let created = a.create_room("开黑", 1000).unwrap();
        (
            a.identity().member_id,
            created.summary.room_id,
            created.code,
        )
    };
    {
        let a = Core::open(storage::open(&path).unwrap(), &keys).unwrap();
        assert_eq!(a.identity().member_id, me);
        assert_eq!(a.identity().nickname, "阿钢");
        let team = a.current_team().unwrap().unwrap();
        assert_eq!(
            (team.room_id, team.title.as_deref(), team.status),
            (room, Some("开黑"), RoomStatus::Active)
        );
        assert_eq!(a.room_code(&room).unwrap(), code);
    }
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

/// 2.0 的队友还会发对线命中（编号 9）：照常入库、能补给别人，队伍照旧，只是不再算层数。
#[test]
fn hit_events_from_2_0_are_kept_but_ignored() {
    let (mut a, mut b, room) = pair();
    let before = b.list_members(&room).unwrap();
    let tx = a.conn.transaction().unwrap();
    let hit = events::append_own(&tx, &a.me, &room, 9, vec![0xa0], 10_000).unwrap();
    tx.commit().unwrap();
    let notes = b.ingest_events(&room, std::slice::from_ref(&hit)).unwrap();
    assert_eq!(notes, vec![CoreEvent::RoomUpdated { room_id: room }]);
    assert!(
        b.room_events(&room).unwrap().contains(&hit),
        "入库了，能补给别人"
    );
    assert_eq!(b.list_members(&room).unwrap(), before);
    assert_eq!(
        b.current_team().unwrap().map(|t| t.status),
        Some(RoomStatus::Active)
    );
}

/// 持有情况有缓存（节点每 100 毫秒要用好几次），每一种写入事件的路径之后都要跟着变：
/// 自己写的、收到队友的、取消加入时整个队伍的事件删掉。
#[test]
fn cached_holdings_follow_every_event_write() {
    let total = |c: &Core, room: &RoomId| -> u64 {
        c.holdings(room)
            .unwrap()
            .vector()
            .iter()
            .map(|e| e.max)
            .sum()
    };
    let (mut a, mut b, room) = pair();
    let before = total(&a, &room);
    assert!(before > 0);
    assert_eq!(total(&a, &room), before, "没写东西：用缓存，结果一样");
    let written = a.rename_room(&room, "新队名", 5000).unwrap();
    assert_eq!(total(&a, &room), before + 1, "自己写了一条");
    let b_before = total(&b, &room);
    b.ingest_events(&room, &written).unwrap();
    assert_eq!(total(&b, &room), b_before + 1, "收到队友的一条");
    b.ingest_events(&room, &written).unwrap();
    assert_eq!(total(&b, &room), b_before + 1, "重复收到不算");

    let mut c = new_core();
    c.join_room(&a.room_code(&room).unwrap(), 6000).unwrap();
    c.ingest_events(&room, &a.room_events(&room).unwrap())
        .unwrap();
    assert!(total(&c, &room) > 0);
    c.cancel_join(&room).unwrap();
    assert_eq!(total(&c, &room), 0, "整个队伍删掉以后不再用旧的缓存");
}
