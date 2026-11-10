//! 节点交出来的通知分给各模块，队伍有变化时刷新队员名字。见 `team` 文件头第 2、5 条。

use std::sync::atomic::Ordering;
use std::time::Instant;

use tauri::{AppHandle, Emitter, Manager};

use super::{PRESENCE_STALE, Seen};
use crate::api::AppState;
use crate::app_core::{AppError, RoomStatus};
use crate::crypto::{MemberId, RoomId, hex};
use crate::emotes::{self, net::NetMsg};
use crate::history::{Kind, NewEntry};
use crate::node::{NodeEvent, now_ms};
use crate::overlay::Msg;
use crate::panel::PanelMsg;
use crate::protocol::live::LiveBody;
use crate::voice::VoiceMsg;
use crate::votes::VoteKind;

/// 现在所在的队伍（加入中或已加入）、是不是已加入、队员名字。
type TeamNames = (Option<RoomId>, bool, Vec<(MemberId, String)>);

/// 把队员名字交给透明窗口（信号下面的标签用），并通知控制面板刷新；告诉历史记录现在是哪个队伍。不在节点任务里等。
pub fn refresh_names(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(state) = app.try_state::<AppState>() else {
            return;
        };
        let node = state.node.clone();
        let names = node
            .call(|core, _| -> Result<TeamNames, AppError> {
                let Some(team) = core.current_team()? else {
                    return Ok((None, false, Vec::new()));
                };
                let active = team.status == RoomStatus::Active;
                Ok((
                    Some(team.room_id),
                    active,
                    core.list_members(&team.room_id)?
                        .into_iter()
                        .map(|m| {
                            (
                                m.member_id,
                                m.nickname
                                    .filter(|n| !n.is_empty())
                                    .unwrap_or(m.fingerprint),
                            )
                        })
                        .collect(),
                ))
            })
            .await;
        if let Some(Ok((room, active, names))) = names {
            // 历史记录只留现在所在的队伍：退出、被踢、解散后这里是 None，记录全删（计划 3.6）
            state.history.set_room(room);
            *state.team.room.lock().expect("队伍锁没有中毒") = room;
            state.panel.send(PanelMsg::Names(names.clone()));
            state.overlays.send(Msg::Names(names));
            if state.team.active.swap(active, Ordering::Relaxed) != active {
                state.panel.send(PanelMsg::InTeam(active));
                crate::api::push_gesture_config(&app);
            }
        }
        let _ = app.emit_to("main", "team://changed", ());
    });
}

/// 节点交出来的通知。在节点任务里调用：只做不等节点的事。
pub fn on_node_event(app: &AppHandle, e: NodeEvent) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    // 收到他一帧正常的实时帧：时间已经对上了，「时间差太多」的提示去掉（文件头第 7 条）
    if let NodeEvent::Live { from, .. } = &e
        && state
            .team
            .skew
            .lock()
            .expect("队伍锁没有中毒")
            .remove(from)
            .is_some()
    {
        let _ = app.emit_to("main", "team://changed", ());
    }
    match e {
        NodeEvent::Live { from, body, .. } => match body {
            LiveBody::Ping { display, x, y, id } => {
                if crate::settings2::PING_IDS.contains(&id.as_str()) {
                    state.history.add(NewEntry {
                        member: Some(from),
                        ts_ms: now_ms(),
                        kind: Kind::Ping,
                        text: id.clone(),
                        voice: None,
                    });
                }
                state.overlays.send(Msg::RemotePing {
                    from,
                    display,
                    x,
                    y,
                    id,
                });
            }
            LiveBody::Emote {
                display,
                x,
                y,
                emote,
                fx,
            } => {
                if emotes::is_ref_shape(&emote) && fx < emotes::FX_VARIANTS {
                    state.history.add(NewEntry {
                        member: Some(from),
                        ts_ms: now_ms(),
                        kind: Kind::Emote,
                        text: emote.clone(),
                        voice: None,
                    });
                    state.overlays.send(Msg::RemoteEmote {
                        from,
                        display,
                        x,
                        y,
                        emote,
                        fx,
                    });
                }
            }
            LiveBody::Text {
                display,
                x,
                y,
                text,
            } => {
                let on = state.settings.lock().expect("设置锁没有中毒").team_text;
                let text = crate::team_text::sanitize(&text);
                if on && !text.is_empty() {
                    state.history.add(NewEntry {
                        member: Some(from),
                        ts_ms: now_ms(),
                        kind: Kind::Text,
                        text: text.clone(),
                        voice: None,
                    });
                    state.overlays.send(Msg::Text {
                        from: Some(from),
                        display,
                        x,
                        y,
                        text,
                    });
                }
            }
            LiveBody::Vote { id, kind, target } => {
                if let Some(kind) = VoteKind::from_code(kind) {
                    state.panel.send(PanelMsg::RemoteVote {
                        from,
                        id,
                        kind,
                        target,
                    });
                }
            }
            LiveBody::Ballot { id, yes } => {
                state.panel.send(PanelMsg::RemoteBallot { from, id, yes })
            }
            LiveBody::Want { to, hash, need } => state.emote_net.send(NetMsg::Want {
                from,
                to,
                hash: hex(&hash),
                need,
            }),
            LiveBody::Chunk { hash, i, n, bytes } => state.emote_net.send(NetMsg::Chunk {
                from,
                hash: hex(&hash),
                i,
                n,
                bytes,
            }),
            LiveBody::Presence {
                status,
                platform,
                emotes: announced,
                off,
                ..
            } => {
                state.emote_net.send(NetMsg::Announced {
                    from,
                    hashes: emotes::split_announced(&announced),
                });
                let mut presence = state.team.presence.lock().expect("在线状态锁没有中毒");
                let changed = presence
                    .get(&from)
                    .is_none_or(|s| s.status != status || s.at.elapsed() > PRESENCE_STALE);
                presence.insert(
                    from,
                    Seen {
                        status,
                        platform,
                        off,
                        at: Instant::now(),
                    },
                );
                drop(presence);
                // 只在状态变了时刷新控制面板，免得每 2 秒刷一次
                if changed {
                    let _ = app.emit_to("main", "team://changed", ());
                }
            }
            LiveBody::Voice {
                stream,
                first,
                frames,
                end,
            } => state.voice.send(VoiceMsg::Remote {
                from,
                stream,
                first,
                frames: frames
                    .into_iter()
                    .map(serde_bytes::ByteBuf::into_vec)
                    .collect(),
                end,
            }),
            LiveBody::Bye => {
                state
                    .team
                    .presence
                    .lock()
                    .expect("在线状态锁没有中毒")
                    .remove(&from);
                state.emote_net.send(NetMsg::Gone(from));
                state.panel.send(PanelMsg::Gone(from));
                state.voice.send(VoiceMsg::Gone(from));
                let _ = app.emit_to("main", "team://changed", ());
            }
        },
        NodeEvent::ProtocolMismatch { remote_proto, .. } => {
            let _ = app.emit_to("main", "team://mismatch", remote_proto);
        }
        NodeEvent::Core(_) | NodeEvent::RoomStatusChanged { .. } => refresh_names(app),
        NodeEvent::ClockSkew { from, skew_ms, .. } => {
            state
                .team
                .skew
                .lock()
                .expect("队伍锁没有中毒")
                .insert(from, (Instant::now(), skew_ms));
            let _ = app.emit_to("main", "team://changed", ());
        }
        NodeEvent::Delivered { .. } => {}
    }
}
