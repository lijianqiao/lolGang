//! 在线状态：本人的（`refresh_presence`）和队伍页上没有事件、却会变的东西（`watch`）。见 `team` 文件头第 3、6 条。

use std::sync::atomic::Ordering;

use tauri::{AppHandle, Emitter, Manager};

use super::{AWAY_AFTER_MS, PRESENCE_STALE, SKEW_FRESH};
use crate::api::AppState;
use crate::crypto::MemberId;
use crate::emotes;
use crate::protocol::live::{LiveBody, Status};
use crate::transport::relay::RelayState;

/// 队伍页上会变、却没有事件的东西，变了才通知控制面板（文件头第 6 条）。在线状态线程里调用，可以等节点。
pub fn watch(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let room = *state.team.room.lock().expect("队伍锁没有中毒");
    let node = state.node.clone();
    let net = tauri::async_runtime::block_on(node.inspect(move |n| {
        (
            room.map(|r| n.member_paths(&r)).unwrap_or_default(),
            n.lan_status().degraded,
            n.relay_status()
                .map(|l| l.iter().filter(|r| r.state == RelayState::Ok).count())
                .unwrap_or(0),
        )
    }));
    let mut fresh: Vec<(MemberId, bool)> = state
        .team
        .presence
        .lock()
        .expect("在线状态锁没有中毒")
        .iter()
        .map(|(m, s)| (*m, s.at.elapsed() < PRESENCE_STALE))
        .collect();
    fresh.sort();
    // 「时间差太多」的提示到期了也要刷新一次
    let mut skewed: Vec<MemberId> = state
        .team
        .skew
        .lock()
        .expect("队伍锁没有中毒")
        .iter()
        .filter(|(_, (at, _))| at.elapsed() < SKEW_FRESH)
        .map(|(m, _)| *m)
        .collect();
    skewed.sort();
    let now = format!("{net:?}{fresh:?}{skewed:?}");
    let mut watched = state.team.watched.lock().expect("队伍锁没有中毒");
    if watched.as_deref() != Some(now.as_str()) {
        *watched = Some(now);
        let _ = app.emit_to("main", "team://changed", ());
    }
}

/// 本人的在线状态，变了才交给节点。
pub fn refresh_presence(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let status = if !state.enabled.load(Ordering::Relaxed) {
        Status::Paused
    } else if crate::input::idle_ms().is_some_and(|ms| ms > AWAY_AFTER_MS) {
        Status::Away
    } else {
        Status::Online
    };
    let (announced, off) = {
        let s = state.settings.lock().expect("设置锁没有中毒");
        (s.announced_emotes(), s.features_off())
    };
    let body = LiveBody::Presence {
        status,
        laning: false,
        version: app.package_info().version.to_string(),
        platform: platform_name().into(),
        emotes: emotes::join_announced(&announced),
        off,
    };
    let mut mine = state.team.mine.lock().expect("在线状态锁没有中毒");
    if mine.as_ref() != Some(&body) {
        *mine = Some(body.clone());
        state.node.set_presence(body);
    }
}

fn platform_name() -> &'static str {
    if cfg!(windows) {
        "win"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}
