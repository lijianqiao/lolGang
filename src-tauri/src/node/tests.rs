//! 多节点集成测试：在 127.0.0.1 上用真实 UDP 互连，定时器缩短到几十毫秒。

use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;
use crate::app_core::{CoreEvent, RoomStatus};
use crate::keystore::MemoryKeyStore;
use crate::protocol::live::Status;
use crate::storage;

/// 先占几个空闲端口再释放，让每个节点事先知道彼此的地址。
pub(super) fn free_ports(n: usize) -> Vec<SocketAddr> {
    let socks: Vec<UdpSocket> = (0..n)
        .map(|_| UdpSocket::bind("127.0.0.1:0").unwrap())
        .collect();
    socks.iter().map(|s| s.local_addr().unwrap()).collect()
}

pub(super) fn fast_config(me: SocketAddr, all: &[SocketAddr]) -> NodeConfig {
    NodeConfig {
        lan: LanConfig {
            bind: me,
            fixed_targets: Some(all.iter().copied().filter(|a| *a != me).collect()),
            ..LanConfig::default()
        },
        have_period: Duration::from_millis(200),
        presence_period: Duration::from_millis(100),
        relay_presence_period: Duration::from_millis(200),
        live_have_delay: (Duration::from_millis(10), Duration::from_millis(50)),
        new_peer_have_delay: (Duration::from_millis(0), Duration::from_millis(20)),
        want_timeout: Duration::from_millis(300),
        want_max_tries: 3,
        backfill_per_sec: 1000,
        tick: Duration::from_millis(20),
        relay_urls: Vec::new(),
        ..NodeConfig::default()
    }
}

pub(super) struct TestNode {
    pub(super) handle: NodeHandle,
    pub(super) events: Arc<Mutex<Vec<NodeEvent>>>,
}

impl TestNode {
    /// 收到过的、来自 `from` 的实时帧。
    pub(super) fn lives_from(&self, from: MemberId) -> Vec<(LiveBody, Via)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                NodeEvent::Live {
                    from: f, body, via, ..
                } if *f == from => Some((body.clone(), *via)),
                _ => None,
            })
            .collect()
    }
}

pub(super) fn temp_db() -> PathBuf {
    let tag = crate::crypto::random_bytes::<8>()
        .map(u64::from_le_bytes)
        .unwrap();
    std::env::temp_dir().join(format!("gang2-node-{tag}.db"))
}

pub(super) fn remove_db(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

async fn start(db: &Path, key: u8, me: SocketAddr, all: &[SocketAddr]) -> TestNode {
    start_with(db, key, fast_config(me, all)).await
}

pub(super) async fn start_with(db: &Path, key: u8, cfg: NodeConfig) -> TestNode {
    let core = Core::open(storage::open(db).unwrap(), &MemoryKeyStore([key; 32])).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink_events = events.clone();
    let sink: Sink = Arc::new(move |e| sink_events.lock().unwrap().push(e));
    let handle = Node::spawn(core, cfg, sink).await.unwrap();
    TestNode { handle, events }
}

/// 每 20 毫秒查一次，5 秒内条件成立就返回 true。
pub(super) async fn eventually<F>(node: &TestNode, check: F) -> bool
where
    F: Fn(&mut Core) -> bool + Send + Sync + Clone + 'static,
{
    for _ in 0..250 {
        let c = check.clone();
        if node.handle.call(move |core, _| c(core)).await == Some(true) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// 每 20 毫秒查一次，5 秒内条件成立就返回 true。
pub(super) async fn wait_for(cond: impl Fn() -> bool) -> bool {
    for _ in 0..250 {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

pub(super) async fn create(node: &TestNode) -> (RoomId, String) {
    node.handle
        .call(|core, out| {
            let c = core.create_room("开黑", now_ms()).unwrap();
            out.broadcast(c.summary.room_id, c.events);
            (c.summary.room_id, c.code)
        })
        .await
        .unwrap()
}

pub(super) async fn join(node: &TestNode, code: &str) {
    let code = code.to_string();
    node.handle
        .call(move |core, _| core.join_room(&code, now_ms()).unwrap())
        .await
        .unwrap();
}

pub(super) async fn member_id(node: &TestNode) -> MemberId {
    node.handle
        .call(|c, _| c.identity().member_id)
        .await
        .unwrap()
}

pub(super) fn status_is(
    room: RoomId,
    want: RoomStatus,
) -> impl Fn(&mut Core) -> bool + Send + Sync + Clone + 'static {
    move |core: &mut Core| core.room_status(&room).ok() == Some(want)
}

/// 这个节点认为在队里的人数（含自己）。
pub(super) fn in_room(
    room: RoomId,
    n: usize,
) -> impl Fn(&mut Core) -> bool + Send + Sync + Clone + 'static {
    move |c: &mut Core| c.members_in(&room).unwrap().len() == n
}

pub(super) fn ping(id: &str) -> LiveBody {
    LiveBody::Ping {
        display: 1,
        x: 1000,
        y: 2000,
        id: id.into(),
    }
}

fn presence(status: Status) -> LiveBody {
    LiveBody::Presence {
        status,
        laning: false,
        version: "2.0.0".into(),
        platform: "win".into(),
        emotes: Vec::new(),
        off: 0,
    }
}

/// 甲建队、乙进队，等两边都看到对方。
async fn pair(da: &Path, db: &Path, addrs: &[SocketAddr]) -> (TestNode, TestNode, RoomId) {
    let a = start(da, 1, addrs[0], addrs).await;
    let b = start(db, 2, addrs[1], addrs).await;
    let (room, code) = create(&a).await;
    join(&b, &code).await;
    assert!(
        eventually(&b, status_is(room, RoomStatus::Active)).await,
        "乙应从 Joining 变成 Active"
    );
    assert!(eventually(&a, in_room(room, 2)).await, "甲应看到乙进队");
    (a, b, room)
}

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_form_a_team_and_ping_each_other() {
    let addrs = free_ports(2);
    let (da, db) = (temp_db(), temp_db());
    let (a, b, _room) = pair(&da, &db, &addrs).await;
    let (a_id, b_id) = (member_id(&a).await, member_id(&b).await);
    // 乙要先知道甲是队员（甲的事件早就补齐了）
    a.handle.send_live(ping("danger"));
    assert!(
        wait_for(|| b
            .lives_from(a_id)
            .iter()
            .any(|(body, _)| *body == ping("danger")))
        .await,
        "乙收到甲的信号"
    );
    b.handle.send_live(ping("omw"));
    assert!(
        wait_for(|| a
            .lives_from(b_id)
            .iter()
            .any(|(body, via)| *body == ping("omw") && *via == Via::Lan))
        .await
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        b.lives_from(a_id)
            .iter()
            .filter(|(body, _)| *body == ping("danger"))
            .count(),
        1,
        "同一帧只交一次"
    );
    assert!(b.lives_from(b_id).is_empty(), "自己的广播不算");

    drop((a, b));
    remove_db(&da);
    remove_db(&db);
}

#[tokio::test(flavor = "multi_thread")]
async fn presence_repeats_and_follows_changes() {
    let addrs = free_ports(2);
    let (da, db) = (temp_db(), temp_db());
    let (a, b, _room) = pair(&da, &db, &addrs).await;
    let a_id = member_id(&a).await;
    a.handle.set_presence(presence(Status::Online));
    assert!(
        wait_for(|| b
            .lives_from(a_id)
            .iter()
            .filter(|(body, _)| *body == presence(Status::Online))
            .count()
            >= 3)
        .await,
        "按周期反复发"
    );
    a.handle.set_presence(presence(Status::Paused));
    assert!(
        wait_for(|| b
            .lives_from(a_id)
            .iter()
            .any(|(body, _)| *body == presence(Status::Paused)))
        .await
    );

    drop((a, b));
    remove_db(&da);
    remove_db(&db);
}

#[tokio::test(flavor = "multi_thread")]
async fn live_frames_from_non_members_and_replays_are_dropped() {
    let addrs = free_ports(3);
    let (da, db) = (temp_db(), temp_db());
    let (a, b, room) = pair(&da, &db, &addrs[..2]).await;
    let key = a
        .handle
        .call(move |c, _| c.room_key(&room).unwrap())
        .await
        .unwrap();
    let sock = UdpSocket::bind(addrs[2]).unwrap();
    let send = |frame: &Frame| {
        for p in seal_packets(&key, &room, frame).unwrap() {
            sock.send_to(&p, addrs[0]).unwrap();
        }
    };
    // 拿到了队伍码、却没进队的人：签名没问题，但不是队员
    let outsider = crate::crypto::Identity::from_secret(&[42u8; 32]);
    send(&Frame::Live(crate::protocol::live::sign_live(
        &outsider,
        &room,
        1,
        now_ms(),
        &ping("danger"),
    )));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        a.lives_from(outsider.member_id()).is_empty(),
        "不是队员的帧不要"
    );
    // 队员的帧被人录下来重发：同一个序号只交一次
    let b_id = member_id(&b).await;
    b.handle.send_live(ping("omw"));
    assert!(wait_for(|| a.lives_from(b_id).len() == 1).await);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(a.lives_from(b_id).len(), 1);

    drop((a, b));
    remove_db(&da);
    remove_db(&db);
}

#[test]
fn test_clock_offset_is_only_for_test_instances() {
    assert_eq!(test_clock_offset(Some(3), Some("660000")), 660_000);
    assert_eq!(test_clock_offset(Some(2), Some(" -660000 ")), -660_000);
    assert_eq!(test_clock_offset(None, Some("660000")), 0, "正式身份不认");
    assert_eq!(test_clock_offset(Some(2), None), 0);
    assert_eq!(test_clock_offset(Some(2), Some("十分钟")), 0);
}

/// 收到过几次「`from` 的时间和本机差太多」的通知。
fn skews_from(node: &TestNode, from: MemberId) -> Vec<i64> {
    node.events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            NodeEvent::ClockSkew {
                from: f, skew_ms, ..
            } if *f == from => Some(*skew_ms),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn clock_skew_is_reported_once_per_member_and_only_for_verified_members() {
    use crate::protocol::live::{MAX_SKEW_MS, sign_live};
    let addrs = free_ports(3);
    let (da, db) = (temp_db(), temp_db());
    let (a, b, room) = pair(&da, &db, &addrs[..2]).await;
    let key = a
        .handle
        .call(move |c, _| c.room_key(&room).unwrap())
        .await
        .unwrap();
    let sock = UdpSocket::bind(addrs[2]).unwrap();
    let send = |frame: &Frame| {
        for p in seal_packets(&key, &room, frame).unwrap() {
            sock.send_to(&p, addrs[0]).unwrap();
        }
    };
    let b_id = member_id(&b).await;
    // 比甲的时间快 11 分钟
    let fast = now_ms() + MAX_SKEW_MS + 60_000;
    let signed_by_b = |seq: u64| {
        let handle = b.handle.clone();
        async move {
            handle
                .call(move |c, _| c.sign_live(&room, seq, fast, &ping("danger")))
                .await
                .unwrap()
        }
    };

    // 签名对不上的（改过时间戳）：不通知，否则外人能冒队员的名让界面乱提示
    let mut forged = signed_by_b(1).await;
    forged.2 += 1;
    send(&Frame::Live(forged));
    // 拿到了队伍码、却没进队的人：签名没问题，但不是队员，也不通知
    let outsider = crate::crypto::Identity::from_secret(&[42u8; 32]);
    send(&Frame::Live(sign_live(
        &outsider,
        &room,
        1,
        fast,
        &ping("danger"),
    )));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(skews_from(&a, b_id).is_empty(), "签名不对的不通知");
    assert!(
        skews_from(&a, outsider.member_id()).is_empty(),
        "不是队员的不通知"
    );

    // 队员的帧，时间差 11 分钟：通知一次，帧丢掉；紧接着的第二帧不再通知
    send(&Frame::Live(signed_by_b(2).await));
    send(&Frame::Live(signed_by_b(3).await));
    assert!(wait_for(|| skews_from(&a, b_id).len() == 1).await);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let skews = skews_from(&a, b_id);
    assert_eq!(skews.len(), 1, "同一个人一分钟内只通知一次");
    assert!(
        (MAX_SKEW_MS as i64..MAX_SKEW_MS as i64 + 120_000).contains(&skews[0]),
        "对方快了约 11 分钟，记成正数：{}",
        skews[0]
    );
    assert!(a.lives_from(b_id).is_empty(), "时间差太多的帧不交给界面");

    drop((a, b));
    remove_db(&da);
    remove_db(&db);
}

#[tokio::test(flavor = "multi_thread")]
async fn late_joiner_catches_up_and_activates() {
    let addrs = free_ports(3);
    let dbs = [temp_db(), temp_db(), temp_db()];
    let a = start(&dbs[0], 1, addrs[0], &addrs).await;
    let b = start(&dbs[1], 2, addrs[1], &addrs).await;
    let (room, code) = create(&a).await;
    join(&b, &code).await;
    assert!(eventually(&a, in_room(room, 2)).await);

    let c = start(&dbs[2], 3, addrs[2], &addrs).await;
    join(&c, &code).await;
    assert!(
        eventually(&c, status_is(room, RoomStatus::Active)).await,
        "丙只靠补齐也能激活"
    );
    assert!(eventually(&c, in_room(room, 3)).await);
    assert!(eventually(&a, in_room(room, 3)).await);
    assert!(eventually(&b, in_room(room, 3)).await);

    drop((a, b, c));
    for d in &dbs {
        remove_db(d);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn restarted_node_reconnects_and_keeps_pinging() {
    let addrs = free_ports(2);
    let (da, db) = (temp_db(), temp_db());
    let (a, b, room) = pair(&da, &db, &addrs).await;
    let b_id = member_id(&b).await;

    drop(b);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let b = start(&db, 2, addrs[1], &addrs).await;
    assert_eq!(
        b.handle
            .call(move |c, _| c.room_status(&room).unwrap())
            .await,
        Some(RoomStatus::Active)
    );
    b.handle.send_live(ping("push"));
    assert!(
        wait_for(|| a
            .lives_from(b_id)
            .iter()
            .any(|(body, _)| *body == ping("push")))
        .await,
        "重启后序号仍然比以前的大，甲照收"
    );

    drop((a, b));
    remove_db(&da);
    remove_db(&db);
}

#[tokio::test(flavor = "multi_thread")]
async fn member_offline_during_dissolve_learns_it_later() {
    let addrs = free_ports(2);
    let (da, db) = (temp_db(), temp_db());
    let (a, b, room) = pair(&da, &db, &addrs).await;

    drop(b);
    tokio::time::sleep(Duration::from_millis(100)).await;
    a.handle
        .call(move |core, out| {
            let written = core.dissolve_room(&room, now_ms()).unwrap();
            out.broadcast(room, written);
        })
        .await
        .unwrap();
    let b = start(&db, 2, addrs[1], &addrs).await;
    assert!(
        eventually(&b, status_is(room, RoomStatus::Dissolved)).await,
        "乙上线后应从甲的回应得知解散"
    );

    drop((a, b));
    remove_db(&da);
    remove_db(&db);
}

#[tokio::test(flavor = "multi_thread")]
async fn busy_lan_port_falls_back_to_a_random_port() {
    let taken = UdpSocket::bind("127.0.0.1:0").unwrap();
    let busy = taken.local_addr().unwrap();
    let free = free_ports(1)[0];
    let (d1, d2) = (temp_db(), temp_db());
    let degraded = start(&d1, 1, busy, &[busy]).await;
    let normal = start(&d2, 2, free, &[free]).await;

    let s = degraded.handle.inspect(|n| n.lan_status()).await.unwrap();
    assert!(s.degraded, "端口被占时照常启动，但报告降级");
    assert_ne!(s.port, busy.port());
    let s = normal.handle.inspect(|n| n.lan_status()).await.unwrap();
    assert_eq!((s.port, s.degraded), (free.port(), false));

    drop((degraded, normal, taken));
    remove_db(&d1);
    remove_db(&d2);
}

#[tokio::test(flavor = "multi_thread")]
async fn unconfirmed_event_is_resent_and_then_marked_sent() {
    let addrs = free_ports(2);
    let (da, db) = (temp_db(), temp_db());
    let quick_resend = NodeConfig {
        resend_after: [
            Duration::from_millis(300),
            Duration::from_millis(600),
            Duration::from_millis(1200),
        ],
        ..fast_config(addrs[0], &addrs)
    };
    let a = start_with(&da, 1, quick_resend).await;
    let b = start(&db, 2, addrs[1], &addrs).await;
    let (room, code) = create(&a).await;
    join(&b, &code).await;
    assert!(eventually(&a, in_room(room, 2)).await);

    // 乙掉线期间甲改队名，第一次广播乙收不到
    drop(b);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let id = a
        .handle
        .call(move |core, out| {
            let written = core.rename_room(&room, "五黑", now_ms()).unwrap();
            let id = written[0].event_id;
            out.broadcast(room, written);
            id
        })
        .await
        .unwrap();
    let b = start(&db, 2, addrs[1], &addrs).await;
    assert!(
        eventually(&b, move |c| c.summary(&room).unwrap().title.as_deref()
            == Some("五黑"))
        .await,
        "重发或补齐后乙收到"
    );
    assert!(
        eventually(&a, move |c| c.pending_events(&room).unwrap().is_empty()).await,
        "乙的 Have 确认后甲标记为已发送"
    );
    assert!(a.events.lock().unwrap().iter().any(|e| matches!(
        e,
        NodeEvent::Delivered { room_id, event_ids } if *room_id == room && event_ids.contains(&id)
    )));

    drop((a, b));
    remove_db(&da);
    remove_db(&db);
}

#[tokio::test(flavor = "multi_thread")]
async fn bye_and_leave_go_out_before_the_team_goes_quiet() {
    let addrs = free_ports(2);
    let (da, db) = (temp_db(), temp_db());
    let (a, b, room) = pair(&da, &db, &addrs).await;
    let b_id = member_id(&b).await;

    b.handle
        .call(move |core, out| {
            out.live(room, LiveBody::Bye);
            let written = core.leave_room(&room, now_ms()).unwrap();
            out.broadcast(room, written);
        })
        .await
        .unwrap();
    assert!(
        wait_for(|| a
            .lives_from(b_id)
            .iter()
            .any(|(body, _)| *body == LiveBody::Bye))
        .await,
        "甲收到乙的 Bye"
    );
    assert!(eventually(&a, in_room(room, 1)).await, "甲收到乙的退出");
    assert_eq!(
        b.handle
            .inspect(move |n| n.member_paths(&room).is_empty())
            .await,
        Some(true),
        "退出后乙不再为这个队伍联网"
    );
    // 乙已经不是队员：它再发的帧甲不收（乙不再为这个队伍联网，也发不出去）
    let before = a.lives_from(b_id).len();
    b.handle.send_live(ping("danger"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(a.lives_from(b_id).len(), before);
    assert!(
        a.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, NodeEvent::Core(CoreEvent::RoomUpdated { .. })))
    );

    drop((a, b));
    remove_db(&da);
    remove_db(&db);
}
