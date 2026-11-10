//! 节点：独占 Core，把控制面板的命令、局域网和中继收包、定时器串在一个后台任务里。
//!
//! 实现流程：
//! 1. 命令：控制面板通过 `NodeHandle::call` 发来一个闭包，在节点任务里拿到 `&mut Core` 执行；
//!    闭包把新写的事件放进 `Outbox`，节点随后用 `Events { live: true }` 广播出去。
//!    `NodeHandle::inspect` 读取节点自己的状态（队员路径、中继状态）。
//! 2. 两条路径：局域网 UDP 始终广播；有中继列表时启动中继连接池（`set_relays` 可以随时换，空列表就不连）。
//!    局域网端口被占（多半是别的软件）时不退出，退回随机端口：局域网只能发不能收，中继照常，界面据此提示。
//!    本人的包在有队员只能经中继联系、或局域网上没人在线时，再发一份到中继。
//! 3. 收包：两条路径的包走同一套处理，只是记下来源。按包头 room_id 找到队伍密钥解密。Events 交给 Core 入库；
//!    Have 记下同伴（局域网地址、向量、每条路径的出现时间和周期），再做抑制、补齐和激活检查；
//!    Want 把对方要的事件放进回应队列，从哪条路径来就从哪条路径回：局域网每秒 50 包，中继每秒 3 包。
//!    Live（实时帧）：验签 → 发送者必须是队员 → 挡重放，通过的交给界面（`NodeEvent::Live`），不入库。
//!    队员的帧因为两边系统时间差超过 10 分钟被丢时，告诉界面一声（`NodeEvent::ClockSkew`，同一个人一分钟一次，3.3.0）。
//! 4. 定时器：Have 局域网 5 秒、中继 30 秒（±20% 抖动）；在线状态局域网 2 秒、中继 5 秒；
//!    每 5 秒看一次网卡有没有变，变了就当作网络恢复：马上发 Have，中继立刻重连；延迟 Have（收到实时事件后 1–5 秒、
//!    发现新同伴后 0–2 秒，期间被别人的 Have 覆盖就取消）；Want 超时换人；
//!    同伴在两条路径上都超过 3 个周期没消息视为离线；Joining 队伍检查能否激活。
//! 5. 送达：本人的新事件广播后排进重发计划，没被确认就在 6、15、30 秒各重发一次；
//!    同伴的 Have 覆盖到的本人事件标记为已发送。启动时把仍未确认的本人事件重新排进计划。
//! 6. 已解散的队伍不主动发 Have；同伴的 Have 显示它还不知道解散时，回一次解散事件（同一同伴 1 分钟内最多一次）。
//! 7. 通知通过 sink 交出去：测试里收集起来检查，程序里转发给控制面板和透明窗口。
//!
//! 注意：Core 里的 SQLite 连接不能被多线程共享，所以等待网络发送时不能借用着 Core；
//! 发包的函数只拿传输层和密钥（`send_frame`），不借用整个节点。

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, oneshot};
use tokio::time::MissedTickBehavior;

use crate::app_core::{Core, CoreEvent, RoomStatus};
use crate::crypto::{EventId, MemberId, RoomId, RoomKey};
use crate::protocol::event::SignedEvent;
use crate::protocol::frame::{Frame, HaveEntry, ShortId, WantRange, WireEvent, short_id};
use crate::protocol::live::{LiveBody, LiveSeq, MAX_SKEW_MS, ReplayGuard, WireLive, open_live};
use crate::protocol::packet::{PacketError, open_packet, peek_room_id, seal_packets};
use crate::sync::path::{self, PathKind, PeerPaths, Via};
use crate::sync::plan::{MAX_WANT_EVENTS, dominates, missing_total, ready_to_activate, state_gaps};
use crate::sync::resend::ResendQueue;
use crate::transport::budget::Priority;
use crate::transport::lan::{Inbox, LanConfig, LanTransport};
use crate::transport::relay::{
    RelayConfig, RelayHandle, RelayHealth, RelayInbox, RelayPacket, RelayPool,
};

/// 节点交给界面的通知。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeEvent {
    Core(CoreEvent),
    RoomStatusChanged {
        room_id: RoomId,
        status: RoomStatus,
    },
    ProtocolMismatch {
        room_id: RoomId,
        remote_proto: u8,
    },
    /// 本人这些事件已被同伴确认持有。
    Delivered {
        room_id: RoomId,
        event_ids: Vec<EventId>,
    },
    /// 队友的一帧实时帧（已验签、是队员、不是重放）。
    Live {
        room_id: RoomId,
        from: MemberId,
        body: LiveBody,
        via: Via,
    },
    /// 队员的实时帧因为时间差太多被丢了（3.3.0）：两台电脑的系统时间差超过 10 分钟，信号、语音互相都收不到。
    /// `skew_ms` 是对方的时间减本机的时间。同一个人一分钟最多通知一次。
    ClockSkew {
        room_id: RoomId,
        from: MemberId,
        skew_ms: i64,
    },
}

pub type Sink = Arc<dyn Fn(NodeEvent) + Send + Sync>;

/// 局域网端口情况。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LanStatus {
    pub port: u16,
    /// 配置的端口被占，用的是随机端口，收不到别人的局域网广播。
    pub degraded: bool,
}

#[derive(Clone, Debug)]
pub struct NodeConfig {
    pub lan: LanConfig,
    /// Have 周期：局域网、中继。
    pub have_period: Duration,
    pub relay_have_period: Duration,
    /// 在线状态的周期：局域网、中继。
    pub presence_period: Duration,
    pub relay_presence_period: Duration,
    pub live_have_delay: (Duration, Duration),
    pub new_peer_have_delay: (Duration, Duration),
    pub want_timeout: Duration,
    pub want_max_tries: u32,
    /// 经局域网回应 Want 的发送速率（包/秒）。
    pub backfill_per_sec: u32,
    /// 经中继回应 Want 的发送速率（包/秒）。
    pub relay_backfill_per_sec: u32,
    pub tick: Duration,
    pub relay: RelayConfig,
    /// 启动时连的中继；空列表表示不连（关了「允许互联网」）。
    pub relay_urls: Vec<String>,
    /// 未确认的本人事件在首次发送后多久重发（6、15、30 秒）。
    pub resend_after: [Duration; 3],
}

impl Default for NodeConfig {
    fn default() -> Self {
        NodeConfig {
            lan: LanConfig::default(),
            have_period: path::LAN_HAVE_PERIOD,
            relay_have_period: path::RELAY_HAVE_PERIOD,
            presence_period: Duration::from_secs(2),
            relay_presence_period: Duration::from_secs(5),
            live_have_delay: (Duration::from_secs(1), Duration::from_secs(5)),
            new_peer_have_delay: (Duration::ZERO, Duration::from_secs(2)),
            want_timeout: Duration::from_secs(5),
            want_max_tries: 3,
            backfill_per_sec: 50,
            relay_backfill_per_sec: 3,
            tick: Duration::from_millis(100),
            relay: RelayConfig::default(),
            relay_urls: Vec::new(),
            resend_after: [
                Duration::from_secs(6),
                Duration::from_secs(15),
                Duration::from_secs(30),
            ],
        }
    }
}

/// 命令执行过程中要发出去的事件和要交给界面的通知。
#[derive(Default)]
pub struct Outbox {
    broadcasts: Vec<(RoomId, Vec<SignedEvent>)>,
    /// 先于事件发出去的实时帧（比如退出前的 Bye）。
    lives: Vec<(RoomId, LiveBody)>,
    pub notes: Vec<NodeEvent>,
}

impl Outbox {
    pub fn broadcast(&mut self, room: RoomId, events: Vec<SignedEvent>) {
        if !events.is_empty() {
            self.broadcasts.push((room, events));
        }
    }

    /// 在这批事件之前，先给这个队伍发一帧实时帧。
    pub fn live(&mut self, room: RoomId, body: LiveBody) {
        self.lives.push((room, body));
    }
}

type Job = Box<dyn FnOnce(&mut Node) -> Outbox + Send>;

#[derive(Clone)]
pub struct NodeHandle {
    jobs: mpsc::Sender<Job>,
}

impl NodeHandle {
    /// 在节点任务里执行 `f` 并取回结果。节点已停止时返回 None。
    pub async fn call<R, F>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&mut Core, &mut Outbox) -> R + Send + 'static,
        R: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let job: Job = Box::new(move |node| {
            let mut out = Outbox::default();
            let _ = tx.send(f(&mut node.core, &mut out));
            out
        });
        self.jobs.send(job).await.ok()?;
        rx.await.ok()
    }

    /// 在节点任务里读取节点自己的状态（队员路径、中继状态）。节点已停止时返回 None。
    pub async fn inspect<R, F>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&Node) -> R + Send + 'static,
        R: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let job: Job = Box::new(move |node| {
            let _ = tx.send(f(node));
            Outbox::default()
        });
        self.jobs.send(job).await.ok()?;
        rx.await.ok()
    }

    /// 给所有已加入的队伍发一帧实时帧（信号、表情……）。不等发完，队列满时丢掉（实时帧丢了就丢了）。
    pub fn send_live(&self, body: LiveBody) {
        let job: Job = Box::new(move |node| {
            let mut out = Outbox::default();
            for room in node.active_rooms() {
                out.live(room, body.clone());
            }
            out
        });
        let _ = self.jobs.try_send(job);
    }

    /// 换掉要定时发的在线状态，并马上发一次。
    pub fn set_presence(&self, presence: LiveBody) {
        let job: Job = Box::new(move |node| {
            node.presence = Some(presence.clone());
            node.next_presence = Instant::now();
            node.next_relay_presence = Instant::now();
            Outbox::default()
        });
        let _ = self.jobs.try_send(job);
    }

    /// 换中继列表（改了「允许互联网」或中继设置）；空列表表示不连。
    pub async fn set_relays(&self, urls: Vec<String>) {
        let job: Job = Box::new(move |node| {
            node.cfg.relay_urls = urls;
            node.apply_relays();
            Outbox::default()
        });
        let _ = self.jobs.send(job).await;
    }
}

pub fn now_ms() -> u64 {
    let real = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    real.saturating_add_signed(test_clock_offset_ms())
}

/// 测试实例（`LOLGANG_INSTANCE=2/3`）可以用 `LOLGANG_TEST_CLOCK_MS` 把自己的时间拨快或拨慢（毫秒，可以是负数），
/// 用来实测「两台电脑时间差太多」：同一台电脑上两个实例的系统时间没法不一样。正式身份不认这个环境变量。
fn test_clock_offset_ms() -> i64 {
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        test_clock_offset(
            crate::dev_instance(),
            std::env::var("LOLGANG_TEST_CLOCK_MS").ok().as_deref(),
        )
    })
}

fn test_clock_offset(instance: Option<u16>, var: Option<&str>) -> i64 {
    match (instance, var) {
        (Some(_), Some(v)) => v.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

fn random_between(lo: Duration, hi: Duration) -> Duration {
    if hi <= lo {
        return lo;
    }
    let span = (hi - lo).as_millis() as u64;
    lo + Duration::from_millis(getrandom::u64().unwrap_or(0) % (span + 1))
}

/// 周期 ±20% 的随机抖动，避免所有人同一时刻发 Have。
fn jitter(base: Duration) -> Duration {
    random_between(base.mul_f64(0.8), base.mul_f64(1.2))
}

/// 包发往哪里。
#[derive(Clone, Copy, Debug)]
enum Route {
    /// 局域网广播；`relay` 不为 None 时再以这个优先级发一份到中继。
    Broadcast { relay: Option<Priority> },
    /// 局域网单播。
    Lan(SocketAddr),
    /// 只发到中继。
    Relay(Priority),
}

/// 包从哪里来。
#[derive(Clone, Copy, Debug)]
enum Source {
    Lan(SocketAddr),
    Relay,
}

impl Source {
    fn via(self) -> Via {
        match self {
            Source::Lan(_) => Via::Lan,
            Source::Relay => Via::Relay,
        }
    }

    /// 回应走原路。
    fn reply(self, priority: Priority) -> Route {
        match self {
            Source::Lan(addr) => Route::Lan(addr),
            Source::Relay => Route::Relay(priority),
        }
    }
}

async fn send_frame(
    lan: &LanTransport,
    relay: Option<&RelayHandle>,
    key: &RoomKey,
    room: &RoomId,
    frame: &Frame,
    route: Route,
) {
    let Ok(packets) = seal_packets(key, room, frame) else {
        return;
    };
    for p in packets {
        match route {
            Route::Broadcast { relay: priority } => {
                lan.broadcast(&p).await;
                if let (Some(r), Some(priority)) = (relay, priority) {
                    r.publish(*room, p, priority);
                }
            }
            Route::Lan(addr) => lan.send_to(addr, &p).await,
            Route::Relay(priority) => {
                if let Some(r) = relay {
                    r.publish(*room, p, priority);
                }
            }
        }
    }
}

async fn recv_relay(inbox: &mut Option<RelayInbox>) -> Option<RelayPacket> {
    match inbox {
        Some(i) => i.recv().await,
        None => std::future::pending().await,
    }
}

/// 实时帧在中继上的优先级：信号这类人要立刻看到的排在前面，在线状态和控制帧一样。
fn live_priority(body: &LiveBody) -> Priority {
    match body {
        LiveBody::Presence { .. } => Priority::Control,
        LiveBody::Ping { .. }
        | LiveBody::Emote { .. }
        | LiveBody::Text { .. }
        | LiveBody::Voice { .. }
        | LiveBody::Vote { .. }
        | LiveBody::Ballot { .. }
        | LiveBody::Bye => Priority::Live,
        // 要图和在线状态一样；图的分块最后发，不挤占信号
        LiveBody::Want { .. } => Priority::Control,
        LiveBody::Chunk { .. } => Priority::Backfill,
    }
}

struct Peer {
    /// 最近一次经局域网联系时的地址。
    lan_addr: Option<SocketAddr>,
    paths: PeerPaths,
    vector: Vec<HaveEntry>,
    last_seen: Instant,
}

struct InFlightWant {
    peer: MemberId,
    sent_at: Instant,
    missing_before: u64,
}

struct RoomSync {
    key: RoomKey,
    status: RoomStatus,
    peers: HashMap<MemberId, Peer>,
    /// 现在的队员，实时帧只收他们的。
    members: HashSet<MemberId>,
    guard: ReplayGuard,
    next_have: Instant,
    next_relay_have: Instant,
    pending_have: Option<Instant>,
    want: Option<InFlightWant>,
    failures: HashMap<MemberId, u32>,
    told_dissolve: HashMap<MemberId, Instant>,
    /// 上一次因为时间差太多通知界面是什么时候（同一个人一分钟一次）。
    skew_told: HashMap<MemberId, Instant>,
}

pub struct Node {
    core: Core,
    lan: LanTransport,
    lan_degraded: bool,
    relay: Option<RelayHandle>,
    relay_in: Option<RelayInbox>,
    cfg: NodeConfig,
    sink: Sink,
    me: MemberId,
    rooms: HashMap<RoomId, RoomSync>,
    backfill: VecDeque<(SocketAddr, Vec<u8>)>,
    relay_backfill: VecDeque<(RoomId, Vec<u8>)>,
    /// 下一个中继补齐包最早什么时候发。
    relay_backfill_at: Instant,
    resend: ResendQueue<SignedEvent>,
    net_signature: Vec<std::net::IpAddr>,
    next_net_check: Instant,
    live_seq: LiveSeq,
    /// 要定时发的在线状态；还没设置时不发。
    presence: Option<LiveBody>,
    next_presence: Instant,
    next_relay_presence: Instant,
}

/// 多久看一次网卡有没有变。
const NET_CHECK: Duration = Duration::from_secs(5);
/// 同一个队员「时间差太多」多久通知界面一次。
const SKEW_NOTE_EVERY: Duration = Duration::from_secs(60);

/// 发给某位同伴的路径：在局域网上就单播，否则经中继（帧里带目标，其他人收到会忽略）。
fn route_to(p: &Peer, now: Instant) -> Route {
    match (p.paths.kind(now), p.lan_addr) {
        (PathKind::Lan, Some(addr)) => Route::Lan(addr),
        _ => Route::Relay(Priority::Control),
    }
}

impl Node {
    pub async fn spawn(core: Core, cfg: NodeConfig, sink: Sink) -> std::io::Result<NodeHandle> {
        let (lan, inbox, lan_degraded) = match LanTransport::bind(&cfg.lan).await {
            Ok((lan, inbox)) => (lan, inbox, false),
            Err(_) => {
                let mut fallback = cfg.lan.clone();
                fallback.bind.set_port(0);
                let (lan, inbox) = LanTransport::bind(&fallback).await?;
                (lan, inbox, true)
            }
        };
        let (tx, rx) = mpsc::channel::<Job>(256);
        let me = core.identity().member_id;
        let cfg_resend = cfg.resend_after;
        let now = Instant::now();
        let mut node = Node {
            core,
            lan,
            lan_degraded,
            relay: None,
            relay_in: None,
            cfg,
            sink,
            me,
            rooms: HashMap::new(),
            backfill: VecDeque::new(),
            relay_backfill: VecDeque::new(),
            relay_backfill_at: now,
            resend: ResendQueue::new(cfg_resend),
            net_signature: crate::transport::lan::interface_signature(),
            next_net_check: now + NET_CHECK,
            live_seq: LiveSeq::default(),
            presence: None,
            next_presence: now,
            next_relay_presence: now,
        };
        node.refresh_rooms();
        node.apply_relays();
        node.queue_pending();
        tokio::spawn(node.run(rx, inbox));
        Ok(NodeHandle { jobs: tx })
    }

    async fn run(mut self, mut jobs: mpsc::Receiver<Job>, mut inbox: Inbox) {
        let mut tick = tokio::time::interval(self.cfg.tick);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                job = jobs.recv() => {
                    let Some(job) = job else { break };
                    let out = job(&mut self);
                    // 先只加新队伍、不移走已退出的：退出时写的 Leave 和 Bye 还要发出去
                    self.sync_rooms(false);
                    for (room, body) in out.lives {
                        self.send_live(room, body).await;
                    }
                    for (room, events) in out.broadcasts {
                        self.send_events(room, events).await;
                    }
                    self.refresh_rooms();
                    for n in out.notes {
                        (self.sink)(n);
                    }
                }
                pkt = inbox.recv() => {
                    let Some((from, bytes)) = pkt else { break };
                    self.on_packet(Source::Lan(from), &bytes).await;
                }
                pkt = recv_relay(&mut self.relay_in) => match pkt {
                    Some(p) => self.on_packet(Source::Relay, &p.packet).await,
                    None => self.relay_in = None,
                },
                _ = tick.tick() => self.on_tick().await,
            }
        }
    }

    /// 和 Core 的队伍列表对齐：新队伍取密钥，已有的更新状态和队员，已退出的移除；队伍有增减时更新中继订阅。
    fn refresh_rooms(&mut self) {
        self.sync_rooms(true);
    }

    /// `prune` 为 false 时不移走已退出的队伍。
    fn sync_rooms(&mut self, prune: bool) {
        let Ok(list) = self.core.sync_rooms() else {
            return;
        };
        let before: HashSet<RoomId> = self.rooms.keys().copied().collect();
        if prune {
            self.rooms.retain(|id, _| list.iter().any(|(r, _)| r == id));
        }
        for (room, status) in list {
            let members: HashSet<MemberId> = self
                .core
                .members_in(&room)
                .unwrap_or_default()
                .into_iter()
                .collect();
            if let Some(rs) = self.rooms.get_mut(&room) {
                rs.status = status;
                rs.members = members;
                continue;
            }
            let Ok(key) = self.core.room_key(&room) else {
                continue;
            };
            let now = Instant::now();
            self.rooms.insert(
                room,
                RoomSync {
                    key,
                    status,
                    peers: HashMap::new(),
                    members,
                    guard: ReplayGuard::default(),
                    next_have: now,
                    next_relay_have: now,
                    pending_have: None,
                    want: None,
                    failures: HashMap::new(),
                    told_dissolve: HashMap::new(),
                    skew_told: HashMap::new(),
                },
            );
        }
        let after: HashSet<RoomId> = self.rooms.keys().copied().collect();
        if let Some(r) = &self.relay
            && after != before
        {
            r.set_rooms(after.into_iter().collect());
        }
    }

    /// 已加入（Active）的队伍。
    fn active_rooms(&self) -> Vec<RoomId> {
        self.rooms
            .iter()
            .filter(|(_, rs)| rs.status == RoomStatus::Active)
            .map(|(r, _)| *r)
            .collect()
    }

    /// 按 `cfg.relay_urls` 重建中继连接池；列表为空时不连。
    fn apply_relays(&mut self) {
        // 先丢弃旧连接池，它的连接随之断开
        self.relay = None;
        self.relay_in = None;
        if self.cfg.relay_urls.is_empty() {
            return;
        }
        let rooms = self.rooms.keys().copied().collect();
        let (handle, inbox) =
            RelayPool::start(self.cfg.relay_urls.clone(), rooms, self.cfg.relay.clone());
        self.relay = Some(handle);
        self.relay_in = Some(inbox);
    }

    /// 某个队伍里每位在线同伴当前的联系路径。
    pub fn member_paths(&self, room: &RoomId) -> Vec<(MemberId, PathKind)> {
        let now = Instant::now();
        self.rooms
            .get(room)
            .map(|rs| {
                rs.peers
                    .iter()
                    .map(|(m, p)| (*m, p.paths.kind(now)))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn lan_status(&self) -> LanStatus {
        LanStatus {
            port: self.lan.local_addr().port(),
            degraded: self.lan_degraded,
        }
    }

    /// 网卡变了，当作网络恢复：所有队伍尽快发 Have，中继不再等退避。
    fn check_network(&mut self, now: Instant) {
        if now < self.next_net_check {
            return;
        }
        self.next_net_check = now + NET_CHECK;
        let sig = crate::transport::lan::interface_signature();
        if sig == self.net_signature {
            return;
        }
        self.net_signature = sig;
        let rooms: Vec<RoomId> = self.rooms.keys().copied().collect();
        let delay = self.cfg.new_peer_have_delay;
        for room in rooms {
            self.schedule_have(&room, delay);
        }
        if let Some(r) = &self.relay {
            r.reconnect_now();
        }
    }

    /// 各中继的连接状态；关闭互联网时为 None。
    pub fn relay_status(&self) -> Option<Vec<RelayHealth>> {
        self.relay.as_ref().map(RelayHandle::status)
    }

    fn key_of(&self, room: &RoomId) -> Option<RoomKey> {
        self.rooms.get(room).map(|r| r.key)
    }

    /// 本人的包除了局域网广播，是否还要发一份到中继。
    fn needs_relay(&self, room: &RoomId) -> bool {
        let now = Instant::now();
        let peers = self
            .rooms
            .get(room)
            .into_iter()
            .flat_map(|rs| rs.peers.values())
            .map(|p| p.paths.kind(now));
        path::also_send_to_relay(self.relay.is_some(), peers)
    }

    /// 启动时把仍未确认的本人事件排进重发计划。
    fn queue_pending(&mut self) {
        let now = Instant::now();
        let rooms: Vec<RoomId> = self.rooms.keys().copied().collect();
        for room in rooms {
            for e in self.core.pending_events(&room).unwrap_or_default() {
                self.resend.add(room, e.event_id, e, now);
            }
        }
    }

    /// 本人新写的事件：实时广播，需要时也发到中继；排进重发计划，等同伴确认。
    async fn send_events(&mut self, room: RoomId, events: Vec<SignedEvent>) {
        let now = Instant::now();
        for e in &events {
            self.resend.add(room, e.event_id, e.clone(), now);
        }
        self.broadcast_events(room, events).await;
    }

    async fn broadcast_events(&mut self, room: RoomId, events: Vec<SignedEvent>) {
        let Some(key) = self.key_of(&room) else {
            return;
        };
        let frame = Frame::Events {
            live: true,
            events: events.iter().map(WireEvent::from_signed).collect(),
        };
        let relay = self.needs_relay(&room).then_some(Priority::Live);
        send_frame(
            &self.lan,
            self.relay.as_ref(),
            &key,
            &room,
            &frame,
            Route::Broadcast { relay },
        )
        .await;
    }

    fn sign_live(&mut self, room: &RoomId, body: &LiveBody) -> WireLive {
        let seq = self.live_seq.next(now_us());
        self.core.sign_live(room, seq, now_ms(), body)
    }

    /// 发一帧实时帧：局域网广播，需要时也发到中继。
    async fn send_live(&mut self, room: RoomId, body: LiveBody) {
        let Some(key) = self.key_of(&room) else {
            return;
        };
        let frame = Frame::Live(self.sign_live(&room, &body));
        let relay = self.needs_relay(&room).then_some(live_priority(&body));
        send_frame(
            &self.lan,
            self.relay.as_ref(),
            &key,
            &room,
            &frame,
            Route::Broadcast { relay },
        )
        .await;
    }

    /// 在线状态：局域网和中继各按自己的周期发。
    async fn send_presence(&mut self, now: Instant) {
        let Some(body) = self.presence.clone() else {
            return;
        };
        let lan_due = now >= self.next_presence;
        let relay_due = self.relay.is_some() && now >= self.next_relay_presence;
        if !lan_due && !relay_due {
            return;
        }
        if lan_due {
            self.next_presence = now + self.cfg.presence_period;
        }
        if relay_due {
            self.next_relay_presence = now + self.cfg.relay_presence_period;
        }
        for room in self.active_rooms() {
            let Some(key) = self.key_of(&room) else {
                continue;
            };
            let frame = Frame::Live(self.sign_live(&room, &body));
            let route = match (lan_due, relay_due && self.needs_relay(&room)) {
                (true, true) => Route::Broadcast {
                    relay: Some(Priority::Control),
                },
                (true, false) => Route::Broadcast { relay: None },
                (false, true) => Route::Relay(Priority::Control),
                (false, false) => continue,
            };
            send_frame(&self.lan, self.relay.as_ref(), &key, &room, &frame, route).await;
        }
    }

    /// 在一条路径上发 Have，周期按这条路径填。
    async fn send_have(&mut self, room: RoomId, via: Via) {
        let (Some(key), Ok(vector)) = (self.key_of(&room), self.core.have_vector(&room)) else {
            return;
        };
        let (period, route) = match via {
            Via::Lan => (self.cfg.have_period, Route::Broadcast { relay: None }),
            Via::Relay => (self.cfg.relay_have_period, Route::Relay(Priority::Control)),
        };
        let period_s = (period.as_millis() as u64).div_ceil(1000).max(1) as u16;
        let frame = Frame::Have {
            from: self.me,
            period_s,
            vector,
        };
        send_frame(&self.lan, self.relay.as_ref(), &key, &room, &frame, route).await;
    }

    fn schedule_have(&mut self, room: &RoomId, (lo, hi): (Duration, Duration)) {
        if let Some(rs) = self.rooms.get_mut(room)
            && rs.status != RoomStatus::Dissolved
            && rs.pending_have.is_none()
        {
            rs.pending_have = Some(Instant::now() + random_between(lo, hi));
        }
    }

    fn emit(&self, e: NodeEvent) {
        (self.sink)(e);
    }

    /// 入库等操作可能改变队伍状态（比如收到解散），变了就刷新并通知界面。
    fn note_status_change(&mut self, room: RoomId, before: Option<RoomStatus>) {
        let after = self.core.room_status(&room).ok();
        if after != before {
            self.refresh_rooms();
            if let Some(status) = after {
                self.emit(NodeEvent::RoomStatusChanged {
                    room_id: room,
                    status,
                });
            }
        }
    }

    async fn on_packet(&mut self, source: Source, bytes: &[u8]) {
        let Some(room) = peek_room_id(bytes) else {
            return;
        };
        let Some(key) = self.key_of(&room) else {
            return; // 不是本机加入的队伍
        };
        let frame = match open_packet(&key, bytes) {
            Ok(f) => f,
            Err(PacketError::ProtocolMismatch(p)) => {
                self.emit(NodeEvent::ProtocolMismatch {
                    room_id: room,
                    remote_proto: p,
                });
                return;
            }
            Err(_) => return,
        };
        match frame {
            Frame::Events { live, events } => self.on_events(room, live, events),
            Frame::Have {
                from: member,
                period_s,
                vector,
            } => self.on_have(room, source, member, period_s, vector).await,
            Frame::Want { to, ranges } => self.on_want(room, source, to, ranges),
            Frame::Live(w) => self.on_live(room, source, &w),
        }
        self.maybe_activate(room).await;
    }

    fn on_events(&mut self, room: RoomId, live: bool, wire: Vec<WireEvent>) {
        let events: Vec<SignedEvent> = wire.into_iter().map(|w| w.into_signed(room)).collect();
        let before = self.core.room_status(&room).ok();
        let Ok(notes) = self.core.ingest_events(&room, &events) else {
            return;
        };
        let got_new = !notes.is_empty();
        for n in notes {
            self.emit(NodeEvent::Core(n));
        }
        if got_new {
            let members = self.core.members_in(&room).unwrap_or_default();
            if let Some(rs) = self.rooms.get_mut(&room) {
                for gone in rs
                    .members
                    .iter()
                    .filter(|m| !members.contains(m))
                    .copied()
                    .collect::<Vec<_>>()
                {
                    rs.guard.forget(&gone);
                    rs.skew_told.remove(&gone);
                }
                rs.members = members.into_iter().collect();
            }
        }
        self.note_status_change(room, before);
        if live && got_new {
            let delay = self.cfg.live_have_delay;
            self.schedule_have(&room, delay);
        }
    }

    /// 实时帧：验签、是队员、不是重放，才交给界面。
    fn on_live(&mut self, room: RoomId, source: Source, w: &WireLive) {
        let Some(opened) = open_live(w, &room) else {
            return;
        };
        if opened.from == self.me {
            return; // 自己的广播绕回来了
        }
        let Some(rs) = self.rooms.get_mut(&room) else {
            return;
        };
        if !rs.members.contains(&opened.from) {
            return;
        }
        let now = now_ms();
        // 时间差太多：帧照旧不要（挡重放要靠时间），但告诉界面一声，不然用户只看到「人在线、信号收不到」。
        // 放在验签和「是队员」之后：外人伪造的帧不能让界面乱提示。
        if opened.ts_ms.abs_diff(now) > MAX_SKEW_MS {
            let due = rs
                .skew_told
                .get(&opened.from)
                .is_none_or(|t| t.elapsed() >= SKEW_NOTE_EVERY);
            if due {
                rs.skew_told.insert(opened.from, Instant::now());
                (self.sink)(NodeEvent::ClockSkew {
                    room_id: room,
                    from: opened.from,
                    skew_ms: opened.ts_ms as i64 - now as i64,
                });
            }
            return;
        }
        if !rs.guard.accept(&opened.from, opened.seq, opened.ts_ms, now) {
            return;
        }
        self.emit(NodeEvent::Live {
            room_id: room,
            from: opened.from,
            body: opened.body,
            via: source.via(),
        });
    }

    async fn on_have(
        &mut self,
        room: RoomId,
        source: Source,
        member: MemberId,
        period_s: u16,
        vector: Vec<HaveEntry>,
    ) {
        if member == self.me {
            return; // 自己的广播绕回来了
        }
        let Ok(mine) = self.core.have_vector(&room) else {
            return;
        };
        let Some(rs) = self.rooms.get_mut(&room) else {
            return;
        };
        let now = Instant::now();
        let new_peer = !rs.peers.contains_key(&member);
        let period = Duration::from_secs(u64::from(period_s.max(1)));
        let peer = rs.peers.entry(member).or_insert_with(|| Peer {
            lan_addr: None,
            paths: PeerPaths::default(),
            vector: Vec::new(),
            last_seen: now,
        });
        peer.vector = vector.clone();
        peer.last_seen = now;
        peer.paths.seen(source.via(), now, Some(period));
        if let Source::Lan(addr) = source {
            peer.lan_addr = Some(addr);
        }
        if let Ok(ids) = self.core.mark_delivered(&room, &vector)
            && !ids.is_empty()
        {
            self.resend.confirm(&ids);
            self.emit(NodeEvent::Delivered {
                room_id: room,
                event_ids: ids,
            });
        }
        let Some(rs) = self.rooms.get_mut(&room) else {
            return;
        };
        if rs.pending_have.is_some() && dominates(&vector, &mine) {
            rs.pending_have = None; // 别人已经替我报了，不用再发（抑制）
        }
        let dissolved = rs.status == RoomStatus::Dissolved;
        if new_peer {
            let delay = self.cfg.new_peer_have_delay;
            self.schedule_have(&room, delay);
        }
        if dissolved {
            self.tell_dissolve(room, member, source, &vector).await;
        }
        self.plan_want(room).await;
    }

    async fn tell_dissolve(
        &mut self,
        room: RoomId,
        member: MemberId,
        source: Source,
        vector: &[HaveEntry],
    ) {
        let Ok(Some(d)) = self.core.dissolve_event(&room) else {
            return;
        };
        let owner = short_id(&d.event.author_id);
        if vector
            .iter()
            .any(|e| e.author == owner && e.max >= d.event.seq)
        {
            return; // 对方已经知道
        }
        let Some(rs) = self.rooms.get_mut(&room) else {
            return;
        };
        if rs
            .told_dissolve
            .get(&member)
            .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
        {
            return;
        }
        rs.told_dissolve.insert(member, Instant::now());
        let key = rs.key;
        let frame = Frame::Events {
            live: false,
            events: vec![WireEvent::from_signed(&d)],
        };
        let route = source.reply(Priority::Backfill);
        send_frame(&self.lan, self.relay.as_ref(), &key, &room, &frame, route).await;
    }

    fn on_want(&mut self, room: RoomId, source: Source, to: ShortId, ranges: Vec<WantRange>) {
        if to != short_id(&self.me) {
            return;
        }
        let Ok(mut events) = self.core.events_for(&room, &ranges) else {
            return;
        };
        events.truncate(MAX_WANT_EVENTS as usize);
        if events.is_empty() {
            return; // 没有就不回复，由对方超时换人
        }
        let Some(key) = self.key_of(&room) else {
            return;
        };
        let frame = Frame::Events {
            live: false,
            events: events.iter().map(WireEvent::from_signed).collect(),
        };
        let Ok(packets) = seal_packets(&key, &room, &frame) else {
            return;
        };
        match source {
            Source::Lan(addr) => self.backfill.extend(packets.into_iter().map(|p| (addr, p))),
            Source::Relay => self
                .relay_backfill
                .extend(packets.into_iter().map(|p| (room, p))),
        }
    }

    /// 没有在途的 Want 时，挑最近联系过、还没失败太多次的同伴，要它有、本机缺的事件。
    async fn plan_want(&mut self, room: RoomId) {
        // 先看有没有要做的（每 100 毫秒都会进来）：有在途的 Want、队伍解散了、没有队友，就不查库
        let idle = self.rooms.get(&room).is_none_or(|rs| {
            rs.want.is_some() || rs.status == RoomStatus::Dissolved || rs.peers.is_empty()
        });
        if idle {
            return;
        }
        let Ok(holdings) = self.core.holdings(&room) else {
            return;
        };
        let Some(rs) = self.rooms.get(&room) else {
            return;
        };
        if rs.want.is_some() || rs.status == RoomStatus::Dissolved {
            return;
        }
        let now = Instant::now();
        let mut candidates: Vec<(&MemberId, &Peer)> = rs
            .peers
            .iter()
            .filter(|(m, _)| rs.failures.get(*m).copied().unwrap_or(0) < self.cfg.want_max_tries)
            .collect();
        candidates.sort_by_key(|(_, p)| std::cmp::Reverse(p.last_seen));
        let choice = candidates.iter().find_map(|(m, p)| {
            let gaps = state_gaps(&holdings, &p.vector);
            (!gaps.is_empty()).then_some((**m, route_to(p, now), gaps))
        });
        let Some((peer, route, gaps)) = choice else {
            return;
        };
        let key = rs.key;
        let missing_before = rs
            .peers
            .get(&peer)
            .map_or(0, |p| missing_total(&holdings, &p.vector));
        let frame = Frame::Want {
            to: short_id(&peer),
            ranges: gaps,
        };
        send_frame(&self.lan, self.relay.as_ref(), &key, &room, &frame, route).await;
        if let Some(rs) = self.rooms.get_mut(&room) {
            rs.want = Some(InFlightWant {
                peer,
                sent_at: Instant::now(),
                missing_before,
            });
        }
    }

    /// 在途的 Want 有进展（缺的总数变少）就放行下一个；超时没进展记一次失败，换人。
    fn check_want(&mut self, room: RoomId) {
        // 没有在途的 Want 就不查库（每 100 毫秒都会进来）
        if self.rooms.get(&room).is_none_or(|rs| rs.want.is_none()) {
            return;
        }
        let Ok(holdings) = self.core.holdings(&room) else {
            return;
        };
        let timeout = self.cfg.want_timeout;
        let Some(rs) = self.rooms.get_mut(&room) else {
            return;
        };
        let Some(w) = &rs.want else {
            return;
        };
        let missing_now = rs
            .peers
            .get(&w.peer)
            .map_or(0, |p| missing_total(&holdings, &p.vector));
        if missing_now < w.missing_before {
            rs.failures.remove(&w.peer);
            rs.want = None;
        } else if w.sent_at.elapsed() > timeout {
            *rs.failures.entry(w.peer).or_default() += 1;
            rs.want = None;
        }
    }

    async fn maybe_activate(&mut self, room: RoomId) {
        let Some(rs) = self.rooms.get(&room) else {
            return;
        };
        if rs.status != RoomStatus::Joining {
            return;
        }
        let peers: Vec<Vec<HaveEntry>> = rs.peers.values().map(|p| p.vector.clone()).collect();
        let (Ok(owner_known), Ok(holdings)) =
            (self.core.owner_known(&room), self.core.holdings(&room))
        else {
            return;
        };
        if !ready_to_activate(owner_known, &holdings, &peers) {
            return;
        }
        let Ok(written) = self.core.activate(&room, now_ms()) else {
            return;
        };
        self.refresh_rooms();
        self.send_events(room, written).await;
        self.emit(NodeEvent::RoomStatusChanged {
            room_id: room,
            status: RoomStatus::Active,
        });
        let delay = self.cfg.new_peer_have_delay;
        self.schedule_have(&room, delay);
        // 刚进队：马上报一次在线状态
        self.next_presence = Instant::now();
        self.next_relay_presence = Instant::now();
    }

    async fn on_tick(&mut self) {
        let now = Instant::now();
        let relay_on = self.relay.is_some();
        let rooms: Vec<RoomId> = self.rooms.keys().copied().collect();
        self.check_network(now);
        for room in rooms {
            let needs_relay = self.needs_relay(&room);
            let (lan_period, relay_period) = (self.cfg.have_period, self.cfg.relay_have_period);
            let (mut lan_have, mut relay_have) = (false, false);
            if let Some(rs) = self.rooms.get_mut(&room) {
                rs.peers
                    .retain(|_, p| p.paths.kind(now) != PathKind::Unknown);
                if rs.status != RoomStatus::Dissolved {
                    let triggered = rs.pending_have.is_some_and(|t| now >= t);
                    if now >= rs.next_have || triggered {
                        rs.next_have = now + jitter(lan_period);
                        rs.pending_have = None;
                        lan_have = true;
                    }
                    if relay_on && (now >= rs.next_relay_have || (triggered && needs_relay)) {
                        rs.next_relay_have = now + jitter(relay_period);
                        relay_have = true;
                    }
                }
            }
            if lan_have {
                self.send_have(room, Via::Lan).await;
            }
            if relay_have {
                self.send_have(room, Via::Relay).await;
            }
            self.check_want(room);
            self.plan_want(room).await;
            self.maybe_activate(room).await;
        }
        self.send_presence(now).await;
        let per_tick =
            (u64::from(self.cfg.backfill_per_sec) * self.cfg.tick.as_millis() as u64 / 1000).max(1);
        for _ in 0..per_tick {
            let Some((addr, p)) = self.backfill.pop_front() else {
                break;
            };
            self.lan.send_to(addr, &p).await;
        }
        self.drain_relay_backfill(now);
        let mut due: HashMap<RoomId, Vec<SignedEvent>> = HashMap::new();
        for (room, e) in self.resend.due(now) {
            due.entry(room).or_default().push(e);
        }
        for (room, events) in due {
            self.broadcast_events(room, events).await;
        }
    }

    /// 中继补齐按固定间隔一个一个发；队列空着时不攒额度，免得来活时一下子发一大串。
    fn drain_relay_backfill(&mut self, now: Instant) {
        let Some(relay) = &self.relay else {
            self.relay_backfill.clear(); // 关闭了互联网，发不出去
            return;
        };
        let interval = Duration::from_secs(1) / self.cfg.relay_backfill_per_sec.max(1);
        while now >= self.relay_backfill_at {
            let Some((room, p)) = self.relay_backfill.pop_front() else {
                self.relay_backfill_at = now;
                break;
            };
            relay.publish(room, p, Priority::Backfill);
            self.relay_backfill_at += interval;
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod relay_tests;
