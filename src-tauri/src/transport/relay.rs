//! 中继连接池（架构 6.4、6.6）：同时连多个公共 Nostr 中继，把包发出去、把订阅收到的包交给节点。
//!
//! 实现流程：
//! 1. 一个调度任务 + 每个中继一个连接任务。调度任务持有发送预算、每个房间的 Nostr 密钥和收包去重表；
//!    连接任务只管一条 WebSocket：连接、订阅、收发、判断健康。`RelayHandle` 被丢弃时所有任务结束、连接断开。
//! 2. 发包：`publish` 进入发送预算的优先级队列；有令牌且至少一个中继可用时取出，用该房间的密钥签成
//!    Nostr 事件，发给挑出来的几个可用中继（只扣一个令牌）。没有可用中继时包留在队列里等。
//!    挑法（3.2.0，所有者看了实测定的，需求文档第 3 节）：还没发满 10 个的中继都发（刚启动时全发，先量出回显）；
//!    其余的按回显成功率从高到低、一样时平均回显快的优先，凑够 3 个。掉线、被限速的不可用，自动由下一个顶上。
//!    收照常全听：别人发到哪个中继都收得到。
//! 3. 收包：同一个事件从多个中继来只交一次（最近 4096 个事件 id）；自己发的事件不交；总共每秒最多 200 个。
//! 4. 订阅：连上后按当前房间列表发 `REQ`；房间列表变化时（`set_rooms`）所有连接重发 `REQ`，同名订阅会被替换。
//! 5. 健康（沿用 lolPing）：
//!    - 发出的事件 25 秒内没从订阅里收到回显，就断开重连；
//!    - 连接失败或断开后等待重连，退避 2 秒起、每次翻倍、最多 60 秒；收到过回显的连接断开后从 2 秒重新算；
//!    - `OK` 回 `rate-limited`：保持连接继续收，但 30 秒内不往这个中继发；
//!    - `OK` 或 `CLOSED` 回 `blocked` 等：本次运行不再使用这个中继；
//!    - 网络恢复时节点调用 `reconnect_now`，正在等待重连的连接立刻重连，退避从头算。
//! 6. 状态表放在共享的 `Mutex` 里，连接任务写、调度任务和 `status()` 读；中继变为可用时唤醒调度任务。
//! 7. 回显统计（3.2.0）：每个中继记发出并等回显的事件数、收到回显的数、最近 50 个的回显成功率和平均往返时间，
//!    队伍页「中继」那一行的说明里能看到（也标出正在发给哪几个）。断线时还没等到的算没回显。
//! 8. TLS（3.3.1）：`wss://` 用 rustls（Rust 写的，Windows 和 macOS 行为一样），不再用系统自带的（Windows 的 SChannel、macOS 已过时的 Secure Transport）。
//!    证书交给系统验证（`rustls-platform-verifier`，和自动更新用的一样）：用系统的证书库，服务器少发了中间证书时系统会自己去取，行为和以前接近。
//!    加密算法用 `ring`（自动更新那边已经在用），整个程序共用一份配置（`tls_config`）。rustls 只支持 TLS 1.2 和 1.3：
//!    默认的 8 个中继都是 TLS 1.3（2026-11-12 量过），用户自己填的只支持老 TLS 的中继会连不上（所有者接受）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, sleep, timeout};
use tokio_tungstenite::Connector;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};

use super::budget::{Outgoing, Priority, SendBudget};
use super::nostr::{
    self, NostrKey, Rejection, RelayMsg, classify_rejection, close_message, event_message,
    req_message,
};
use crate::crypto::RoomId;

const SUB_ID: &str = "hs";
const DEDUP_CAP: usize = 4096;
const MAX_PENDING_ECHOES: usize = 64;
/// 回显成功率看最近这么多个事件。
const RECENT_ECHOES: usize = 50;
/// 一个中继发满这么多个事件之前一直发给它，好量出它的回显。
const LEARN_EVENTS: u64 = 10;

#[derive(Clone, Debug)]
pub struct RelayConfig {
    /// 发出事件后多久收不到回显就重连。
    pub echo_timeout: Duration,
    /// 重连退避：(起始, 上限)。
    pub backoff: (Duration, Duration),
    /// 收到 `rate-limited` 后暂停发送多久。
    pub cooldown: Duration,
    pub connect_timeout: Duration,
    /// 订阅从多久之前开始（临时事件中继不保存，这里只是防时钟误差）。
    pub since_back: Duration,
    /// 每秒最多交给节点多少个包。
    pub recv_per_sec: u32,
    /// 量完回显以后每个包发给几个中继（见文件头第 2 条）。
    pub fanout: usize,
}

impl Default for RelayConfig {
    fn default() -> Self {
        RelayConfig {
            echo_timeout: Duration::from_secs(25),
            backoff: (Duration::from_secs(2), Duration::from_secs(60)),
            cooldown: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            since_back: Duration::from_secs(10),
            recv_per_sec: 200,
            fanout: 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayState {
    /// 正在连接，或断开后等待重连。
    Connecting,
    /// 已连接并订阅，可以发。
    Ok,
    /// 被限流，暂停发送（仍在收）。
    Cooling,
    /// 本次运行不再使用。
    Retired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayHealth {
    pub url: String,
    pub state: RelayState,
    /// 最近一次回显的往返时间（毫秒）。
    pub last_echo_ms: Option<u64>,
    /// 本次运行发出并等回显的事件数、其中收到回显的数（3.2.0）。
    pub sent: u64,
    pub echoed: u64,
    /// 最近 `RECENT_ECHOES` 个有结果的事件里收到回显的百分比；还没有结果时是 None。
    /// 没回显的算法：25 秒没来、被中继拒收、连接断了还没来。
    pub echo_pct: Option<u8>,
    /// 回显往返时间的平滑平均（毫秒，新的占 1/8）。
    pub echo_avg_ms: Option<u64>,
    /// 现在的包发给它。
    pub sending: bool,
}

/// 这次发给哪几个中继（见文件头第 2 条）：可用的里面，还没发满 `LEARN_EVENTS` 个的都发；
/// 其余的按回显成功率从高到低、平均回显从快到慢排，补到 `fanout` 个。
fn pick_targets(h: &[RelayHealth], fanout: usize) -> Vec<usize> {
    let usable = (0..h.len()).filter(|&i| h[i].state == RelayState::Ok);
    let (mut out, mut known): (Vec<usize>, Vec<usize>) =
        usable.partition(|&i| h[i].sent < LEARN_EVENTS);
    known.sort_by_key(|&i| {
        (
            std::cmp::Reverse(h[i].echo_pct.unwrap_or(0)),
            h[i].echo_avg_ms.unwrap_or(u64::MAX),
        )
    });
    let room = fanout.saturating_sub(out.len());
    out.extend(known.into_iter().take(room));
    out.sort_unstable();
    out
}

/// 从中继收到的包。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayPacket {
    pub room: RoomId,
    pub packet: Vec<u8>,
}

pub type RelayInbox = mpsc::Receiver<RelayPacket>;

enum Cmd {
    Publish(Priority, Outgoing),
    SetRooms(Vec<RoomId>),
}

/// 发给某个中继的事件：(事件 id, `EVENT` 消息文本)。
type ToRelay = (String, Utf8Bytes);

/// 连接任务交给调度任务的事件：(事件 id, 发布者公钥, 房间, 包)。
type FromRelay = (String, String, RoomId, Vec<u8>);

type Health = Arc<Mutex<Vec<RelayHealth>>>;

pub struct RelayHandle {
    cmds: mpsc::Sender<Cmd>,
    health: Health,
    wake: Arc<Notify>,
}

impl RelayHandle {
    /// 包进入发送队列；队列满时丢弃（实时事件对方会经 Have 补齐）。
    pub fn publish(&self, room: RoomId, packet: Vec<u8>, priority: Priority) {
        let _ = self
            .cmds
            .try_send(Cmd::Publish(priority, Outgoing { room, packet }));
    }

    pub fn set_rooms(&self, rooms: Vec<RoomId>) {
        let _ = self.cmds.try_send(Cmd::SetRooms(rooms));
    }

    /// 网络刚恢复：正在等待重连的连接不再等，立刻重连。
    pub fn reconnect_now(&self) {
        self.wake.notify_waiters();
    }

    pub fn status(&self) -> Vec<RelayHealth> {
        self.health.lock().map(|h| h.clone()).unwrap_or_default()
    }
}

pub struct RelayPool;

impl RelayPool {
    /// 启动连接池（需要在 tokio 运行时里调用）。
    pub fn start(
        urls: Vec<String>,
        rooms: Vec<RoomId>,
        cfg: RelayConfig,
    ) -> (RelayHandle, RelayInbox) {
        let health: Health = Arc::new(Mutex::new(
            urls.iter()
                .map(|url| RelayHealth {
                    url: url.clone(),
                    state: RelayState::Connecting,
                    last_echo_ms: None,
                    sent: 0,
                    echoed: 0,
                    echo_pct: None,
                    echo_avg_ms: None,
                    sending: false,
                })
                .collect(),
        ));
        let (cmd_tx, cmd_rx) = mpsc::channel(256);
        let (in_tx, in_rx) = mpsc::channel(1024);
        let (from_tx, from_rx) = mpsc::channel(1024);
        let (rooms_tx, rooms_rx) = watch::channel(rooms.clone());
        let ready = Arc::new(Notify::new());
        let wake = Arc::new(Notify::new());
        let mut relays = Vec::new();
        let mut tasks = Vec::new();
        for (index, url) in urls.into_iter().enumerate() {
            let (tx, rx) = mpsc::channel(64);
            relays.push(tx);
            let conn = Connection {
                index,
                url,
                cfg: cfg.clone(),
                health: health.clone(),
                ready: ready.clone(),
                wake: wake.clone(),
                rooms: rooms_rx.clone(),
                inbound: from_tx.clone(),
                recent: VecDeque::new(),
            };
            tasks.push(tokio::spawn(conn.run(rx)));
        }
        let mut sched = Scheduler {
            cfg,
            health: health.clone(),
            ready,
            relays,
            _tasks: Tasks(tasks),
            rooms: rooms_tx,
            keys: HashMap::new(),
            own: HashSet::new(),
            budget: SendBudget::default(),
            started: Instant::now(),
            seen: HashSet::new(),
            seen_order: VecDeque::new(),
            window: (0, 0),
            to_node: in_tx,
        };
        sched.set_rooms(rooms);
        tokio::spawn(sched.run(cmd_rx, from_rx));
        (
            RelayHandle {
                cmds: cmd_tx,
                health,
                wake,
            },
            in_rx,
        )
    }
}

/// 调度任务结束（包括被中止）时一并中止所有连接任务，连接随之断开。
struct Tasks(Vec<JoinHandle<()>>);

impl Drop for Tasks {
    fn drop(&mut self) {
        for t in &self.0 {
            t.abort();
        }
    }
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 连中继用的 TLS 配置（文件头第 8 条）。第一次用到时建，之后共用；建不起来（系统证书验证初始化失败）时每次连接都直接失败、照常退避重连。
fn tls_config() -> Result<&'static Arc<rustls::ClientConfig>, &'static str> {
    static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| build_tls_config().map(Arc::new))
        .as_ref()
        .map_err(String::as_str)
}

fn build_tls_config() -> Result<rustls::ClientConfig, String> {
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_platform_verifier()
        .map_err(|e| e.to_string())?
        .with_no_client_auth())
}

fn set_state(health: &Health, index: usize, state: RelayState) {
    if let Ok(mut h) = health.lock() {
        h[index].state = state;
    }
}

struct Scheduler {
    cfg: RelayConfig,
    health: Health,
    ready: Arc<Notify>,
    relays: Vec<mpsc::Sender<ToRelay>>,
    _tasks: Tasks,
    rooms: watch::Sender<Vec<RoomId>>,
    keys: HashMap<RoomId, NostrKey>,
    own: HashSet<String>,
    budget: SendBudget,
    started: Instant,
    seen: HashSet<String>,
    seen_order: VecDeque<String>,
    /// 收包限速：(当前秒, 这一秒已交出的个数)。
    window: (u64, u32),
    to_node: mpsc::Sender<RelayPacket>,
}

impl Scheduler {
    async fn run(mut self, mut cmds: mpsc::Receiver<Cmd>, mut inbound: mpsc::Receiver<FromRelay>) {
        let ready = self.ready.clone();
        loop {
            // 没有可用中继时不定时醒来，等连接任务通知
            let wait = if self.usable().is_empty() {
                None
            } else {
                self.budget.wait_ms(self.now_ms())
            };
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    None => break,
                    Some(Cmd::Publish(p, out)) => {
                        let now = self.now_ms();
                        self.budget.push(p, out, now);
                    }
                    Some(Cmd::SetRooms(rooms)) => self.set_rooms(rooms),
                },
                Some(msg) = inbound.recv() => self.on_inbound(msg),
                _ = ready.notified() => {}
                _ = sleep(Duration::from_millis(wait.unwrap_or(0))), if wait.is_some() => {}
            }
            self.flush();
        }
    }

    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn set_rooms(&mut self, rooms: Vec<RoomId>) {
        self.keys.retain(|r, _| rooms.contains(r));
        for r in &rooms {
            if !self.keys.contains_key(r)
                && let Ok(k) = NostrKey::generate()
            {
                self.keys.insert(*r, k);
            }
        }
        self.own = self
            .keys
            .values()
            .map(|k| k.pubkey_hex().to_string())
            .collect();
        self.rooms.send_replace(rooms);
    }

    fn usable(&self) -> Vec<usize> {
        let Ok(h) = self.health.lock() else {
            return Vec::new();
        };
        (0..h.len())
            .filter(|&i| h[i].state == RelayState::Ok)
            .collect()
    }

    /// 挑这次发给哪几个中继，顺便在状态表里标出来（队伍页显示）。
    fn targets(&self) -> Vec<usize> {
        let Ok(mut h) = self.health.lock() else {
            return Vec::new();
        };
        let targets = pick_targets(&h, self.cfg.fanout);
        for (i, r) in h.iter_mut().enumerate() {
            r.sending = targets.contains(&i);
        }
        targets
    }

    /// 有令牌、有可用中继时把队列里的包发出去。
    fn flush(&mut self) {
        loop {
            let targets = self.targets();
            if targets.is_empty() {
                return;
            }
            let now = self.now_ms();
            let Some(out) = self.budget.pop(now) else {
                return;
            };
            let Some(key) = self.keys.get(&out.room) else {
                continue; // 已经退出的房间
            };
            let Ok(event) = nostr::build_event(key, &out.room, &out.packet, unix_secs()) else {
                continue;
            };
            let text = Utf8Bytes::from(event_message(&event));
            for i in targets {
                let _ = self.relays[i].try_send((event.id.clone(), text.clone()));
            }
        }
    }

    fn on_inbound(&mut self, (event_id, pubkey, room, packet): FromRelay) {
        if self.own.contains(&pubkey) || !self.keys.contains_key(&room) {
            return;
        }
        if !self.seen.insert(event_id.clone()) {
            return;
        }
        self.seen_order.push_back(event_id);
        if self.seen_order.len() > DEDUP_CAP
            && let Some(old) = self.seen_order.pop_front()
        {
            self.seen.remove(&old);
        }
        let sec = self.now_ms() / 1000;
        if self.window.0 != sec {
            self.window = (sec, 0);
        }
        if self.window.1 >= self.cfg.recv_per_sec {
            return;
        }
        self.window.1 += 1;
        let _ = self.to_node.try_send(RelayPacket { room, packet });
    }
}

/// 一次连接为什么结束。
enum End {
    /// 重连；`healthy` 表示这次连接收到过回显，退避从头算。
    Reconnect {
        healthy: bool,
    },
    Retire,
    Shutdown,
}

struct Connection {
    index: usize,
    url: String,
    cfg: RelayConfig,
    health: Health,
    ready: Arc<Notify>,
    wake: Arc<Notify>,
    rooms: watch::Receiver<Vec<RoomId>>,
    inbound: mpsc::Sender<FromRelay>,
    /// 最近有结果的事件收没收到回显（跨重连保留）。
    recent: VecDeque<bool>,
}

impl Connection {
    async fn run(mut self, mut out: mpsc::Receiver<ToRelay>) {
        let (min, max) = self.cfg.backoff;
        let mut backoff = min;
        let mut pending = VecDeque::new();
        loop {
            self.set(RelayState::Connecting);
            let end = self.session(&mut out, &mut pending).await;
            // 连接断了还没等到的回显不会再来了
            for _ in pending.drain(..) {
                self.outcome(false, None);
            }
            match end {
                End::Retire => {
                    self.set(RelayState::Retired);
                    return;
                }
                End::Shutdown => return,
                End::Reconnect { healthy } => {
                    if healthy {
                        backoff = min;
                    }
                }
            }
            self.set(RelayState::Connecting);
            while out.try_recv().is_ok() {} // 断开期间积下的事件已经过时
            let woken = tokio::select! {
                _ = sleep(backoff) => false,
                _ = self.wake.notified() => true,
            };
            backoff = if woken { min } else { (backoff * 2).min(max) };
        }
    }

    fn set(&self, state: RelayState) {
        set_state(&self.health, self.index, state);
        if state == RelayState::Ok {
            self.ready.notify_one();
        }
    }

    fn req(&mut self) -> Option<String> {
        let rooms = self.rooms.borrow_and_update().clone();
        if rooms.is_empty() {
            return None;
        }
        let since = unix_secs().saturating_sub(self.cfg.since_back.as_secs());
        Some(req_message(SUB_ID, &rooms, since))
    }

    /// `pending`：发出去还在等回显的事件；由 `run` 持有，连接结束时剩下的算没回显。
    async fn session(
        &mut self,
        out: &mut mpsc::Receiver<ToRelay>,
        pending: &mut VecDeque<(String, Instant)>,
    ) -> End {
        let ws_cfg = WebSocketConfig::default()
            .max_message_size(Some(256 << 10))
            .max_frame_size(Some(256 << 10));
        let Ok(tls) = tls_config() else {
            return End::Reconnect { healthy: false };
        };
        let connect = tokio_tungstenite::connect_async_tls_with_config(
            self.url.as_str(),
            Some(ws_cfg),
            true,
            Some(Connector::Rustls(tls.clone())),
        );
        let Ok(Ok((ws, _))) = timeout(self.cfg.connect_timeout, connect).await else {
            return End::Reconnect { healthy: false };
        };
        let (mut sink, mut source) = ws.split();
        if let Some(req) = self.req()
            && sink.send(Message::text(req)).await.is_err()
        {
            return End::Reconnect { healthy: false };
        }
        self.set(RelayState::Ok);

        let mut healthy = false;
        let mut cooling_until: Option<Instant> = None;
        let mut check =
            tokio::time::interval((self.cfg.echo_timeout / 4).min(Duration::from_secs(1)));
        check.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            // 按顺序检查：房间列表变了先重发订阅，再发新事件。否则新房间的第一批事件可能先于订阅到达中继，
            // 收不到回显，25 秒后被误判为不健康而重连。发送排在接收前面：发送受预算限制，不会饿死接收。
            tokio::select! {
                biased;
                changed = self.rooms.changed() => {
                    if changed.is_err() {
                        return End::Shutdown;
                    }
                    let msg = self.req().unwrap_or_else(|| close_message(SUB_ID));
                    if sink.send(Message::text(msg)).await.is_err() {
                        return End::Reconnect { healthy };
                    }
                }
                item = out.recv() => {
                    let Some((id, text)) = item else {
                        return End::Shutdown;
                    };
                    if sink.send(Message::Text(text)).await.is_err() {
                        return End::Reconnect { healthy };
                    }
                    // 判断超时只看最早的那个；积压太多时不再记新的（也不算进回显成功率）
                    if pending.len() < MAX_PENDING_ECHOES {
                        pending.push_back((id, Instant::now()));
                        self.count_sent();
                    }
                }
                msg = source.next() => {
                    let text = match msg {
                        Some(Ok(Message::Text(t))) => t,
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                            return End::Reconnect { healthy };
                        }
                        Some(Ok(_)) => continue,
                    };
                    match nostr::parse_relay_message(text.as_str()) {
                        RelayMsg::Event { event_id, pubkey, room_id, packet, .. } => {
                            if let Some(pos) = pending.iter().position(|(id, _)| *id == event_id) {
                                if let Some((_, sent)) = pending.remove(pos) {
                                    self.outcome(true, Some(sent.elapsed()));
                                }
                                healthy = true;
                            }
                            let _ = self.inbound.try_send((event_id, pubkey, room_id, packet));
                        }
                        RelayMsg::Ok { event_id, accepted, message } => {
                            if !accepted || message.starts_with("duplicate:") {
                                // 不会有回显：拒收的算没回显，重复的不算
                                let before = pending.len();
                                pending.retain(|(id, _)| *id != event_id);
                                if !accepted && pending.len() < before {
                                    self.outcome(false, None);
                                }
                            }
                            if !accepted {
                                match classify_rejection(&message) {
                                    Rejection::Retire => return End::Retire,
                                    Rejection::RateLimited => {
                                        cooling_until = Some(Instant::now() + self.cfg.cooldown);
                                        self.set(RelayState::Cooling);
                                    }
                                    Rejection::Other => {}
                                }
                            }
                        }
                        RelayMsg::Closed(reason) => {
                            if classify_rejection(&reason) == Rejection::Retire {
                                return End::Retire;
                            }
                            return End::Reconnect { healthy }; // 订阅没了，重连重新订阅
                        }
                        _ => {}
                    }
                }
                // 只在等着回显或者在冷却时才每秒查（3.2.0：没事时不定时醒）
                _ = check.tick(), if !pending.is_empty() || cooling_until.is_some() => {
                    if pending.front().is_some_and(|(_, t)| t.elapsed() > self.cfg.echo_timeout) {
                        return End::Reconnect { healthy: false };
                    }
                    if cooling_until.is_some_and(|t| Instant::now() >= t) {
                        cooling_until = None;
                        self.set(RelayState::Ok);
                    }
                }
            }
        }
    }

    fn count_sent(&self) {
        if let Ok(mut h) = self.health.lock() {
            h[self.index].sent += 1;
        }
    }

    /// 一个等回显的事件有了结果：收到了（带往返时间）或者不会来了。
    fn outcome(&mut self, echoed: bool, rtt: Option<Duration>) {
        if self.recent.len() >= RECENT_ECHOES {
            self.recent.pop_front();
        }
        self.recent.push_back(echoed);
        let ok = self.recent.iter().filter(|e| **e).count();
        if let Ok(mut h) = self.health.lock() {
            let me = &mut h[self.index];
            me.echo_pct = Some((ok * 100 / self.recent.len()) as u8);
            if echoed {
                me.echoed += 1;
            }
            if let Some(rtt) = rtt {
                let ms = rtt.as_millis() as u64;
                me.last_echo_ms = Some(ms);
                me.echo_avg_ms = Some(me.echo_avg_ms.map_or(ms, |avg| (avg * 7 + ms) / 8));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TLS 配置建得起来：用的是 ring、只开 TLS 1.2 和 1.3、证书交给系统验证。
    #[test]
    fn tls_config_uses_ring_and_the_system_verifier() {
        let cfg = tls_config().expect("TLS 配置建得起来");
        assert!(
            !cfg.crypto_provider().cipher_suites.is_empty(),
            "ring 提供了加密套件"
        );
        // 第二次拿到的是同一份
        assert!(Arc::ptr_eq(cfg, tls_config().unwrap()));
        // 不要客户端证书；ALPN 不设（WebSocket 不需要）
        assert!(cfg.alpn_protocols.is_empty());
    }

    fn relay(state: RelayState, sent: u64, pct: u8, avg: u64) -> RelayHealth {
        RelayHealth {
            url: String::new(),
            state,
            last_echo_ms: Some(avg),
            sent,
            echoed: sent,
            echo_pct: Some(pct),
            echo_avg_ms: Some(avg),
            sending: false,
        }
    }

    #[test]
    fn relays_still_being_measured_all_get_the_packet() {
        let h: Vec<_> = (0..8).map(|i| relay(RelayState::Ok, i, 100, 300)).collect();
        assert_eq!(pick_targets(&h, 3), (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn measured_relays_are_ranked_by_echo_then_speed() {
        let h = vec![
            relay(RelayState::Ok, 50, 100, 450),
            relay(RelayState::Ok, 50, 90, 100), // 回显差：快也排后面
            relay(RelayState::Ok, 50, 100, 200),
            relay(RelayState::Ok, 50, 100, 380),
            relay(RelayState::Cooling, 50, 100, 50), // 被限速：不发
            relay(RelayState::Ok, 50, 100, 600),
        ];
        assert_eq!(pick_targets(&h, 3), vec![0, 2, 3]);
        // 最好的掉线了：下一个顶上
        let mut h = h;
        h[2].state = RelayState::Connecting;
        assert_eq!(pick_targets(&h, 3), vec![0, 3, 5]);
    }

    #[test]
    fn a_newly_connected_relay_is_measured_alongside_the_best() {
        let mut h: Vec<_> = (0..5)
            .map(|i| relay(RelayState::Ok, 50, 100, 100 + i as u64))
            .collect();
        h.push(relay(RelayState::Ok, 0, 0, 0));
        h[5].echo_pct = None;
        h[5].echo_avg_ms = None;
        assert_eq!(pick_targets(&h, 3), vec![0, 1, 5]);
        assert_eq!(
            pick_targets(&h[..2], 3),
            vec![0, 1],
            "可用的不够 3 个就全发"
        );
        assert!(pick_targets(&[], 3).is_empty());
    }
}
