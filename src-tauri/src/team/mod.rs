//! 队伍的界面接口：控制面板的「队伍」页和个人资料调这里；节点交出来的通知也在这里分发。
//!
//! 实现流程：
//! 1. 命令都是异步的：在节点任务里执行（`NodeHandle::call`），完了返回整份 `TeamView`，控制面板直接换上。
//!    退出队伍时先发一帧 Bye 再写 Leave，队友马上就能看到人走了。
//! 2. 节点的通知（`on_node_event`）在节点任务里调用，不能等节点：
//!    队友的信号、表情、文字交给透明窗口（同时记进历史记录），导入表情的传输帧交给 `emotes::net`，投票交给 `panel`，语音交给 `voice`；在线状态记进 `TeamState`；队伍有变化（进队、退出、改名、资料、解散）就
//!    另开一个异步任务把队员名字交给透明窗口、通知控制面板刷新。
//! 3. 本人的在线状态（`refresh_presence`）：信号关着是「暂停」，5 分钟没碰键鼠是「离开」，否则「在线」。
//!    变了才交给节点，节点自己按周期重复发。
//! 4. 队友的在线状态 15 秒（中继 3 个周期）没更新就当作没有。投票人是本人加上在线状态新鲜、没关投票的队友。
//! 5. 进了队伍或不在队伍里了（`refresh_names` 查到的状态变了），就让手势重新拿配置：文字快捷键和面板键只在队伍里拦。
//! 6. 队伍页上会变、却没有事件的东西（队友走局域网还是中继、掉线、在线状态过期、连着几个中继、局域网端口）由在线状态线程
//!    每 5 秒看一次（`watch`），变了才通知控制面板刷新（3.0-M6：以前是控制面板自己每 3 秒取一遍整份队伍页）。
//! 7. 时间差太多（3.3.0）：队友的电脑时间和本机差超过 10 分钟时，他的实时帧节点一帧都不收（挡重放要靠时间），
//!    表现是「人在队里、信号和语音互相收不到」。节点为此发 `ClockSkew`，这里记下来，队伍页在那个人那一行提示；
//!    收到他一帧正常的实时帧（时间校准了）就去掉，他不再发帧的话 90 秒后不再提示。
//!
//! 文件（3.2.0 从一个 team.rs 拆开，只搬代码）：`view.rs` 拼队伍页，`commands.rs` 队伍命令和个人资料，
//! `presence.rs` 在线状态（第 3、6 条），`notify.rs` 分发节点的通知（第 2、5 条）。这里留队伍状态和投票人。

pub mod commands;
pub mod notify;
pub mod presence;
pub mod view;

pub use notify::{on_node_event, refresh_names};
pub use presence::{refresh_presence, watch};

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

use crate::api::AppState;
use crate::crypto::{MemberId, RoomId};
use crate::protocol::live::{LiveBody, OFF_VOTES, Status};

/// 多久没碰键鼠算离开电脑。
const AWAY_AFTER_MS: u64 = 5 * 60 * 1000;
/// 队友的在线状态多久没更新就不算了。
const PRESENCE_STALE: Duration = Duration::from_secs(15);
/// 「时间差太多」的提示留多久。节点对同一个人一分钟通知一次，留得比一分钟长一点，提示才不会一闪一闪。
const SKEW_FRESH: Duration = Duration::from_secs(90);

/// 队友最近一次的在线状态。
struct Seen {
    status: Status,
    platform: String,
    /// 关掉了的功能（`OFF_TEXT`、`OFF_VOTES`）。
    off: u8,
    at: Instant,
}

#[derive(Default)]
pub struct TeamState {
    presence: Mutex<HashMap<MemberId, Seen>>,
    /// 最近一次交给节点的本人在线状态。
    mine: Mutex<Option<LiveBody>>,
    /// 在一个已加入的队伍里。
    pub active: AtomicBool,
    /// 现在所在的队伍（加入中或已加入），`refresh_names` 记下，`watch` 用。
    room: Mutex<Option<RoomId>>,
    /// `watch` 上一次看到的样子。
    watched: Mutex<Option<String>>,
    /// 时间和本机差太多的队友：什么时候报的、差多少毫秒（对方减本机）。收到他一帧正常的实时帧就去掉。
    skew: Mutex<HashMap<MemberId, (Instant, i64)>>,
}

/// 投票人：本人加上在线、没关投票的队友。
pub fn voters(app: &AppHandle, me: MemberId) -> Vec<MemberId> {
    let mut out = vec![me];
    if let Some(state) = app.try_state::<AppState>() {
        let presence = state.team.presence.lock().expect("在线状态锁没有中毒");
        out.extend(
            presence
                .iter()
                .filter(|(m, s)| {
                    **m != me && s.at.elapsed() <= PRESENCE_STALE && s.off & OFF_VOTES == 0
                })
                .map(|(m, _)| *m),
        );
    }
    out
}
