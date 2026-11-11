//! 聊天框：打开、收起、交来的一句话（指令或发给队伍的文字）。见 `panel` 文件头第 4 条。

use tauri::{AppHandle, Emitter, State};
use tokio::sync::oneshot;

use super::{CHAT_MARGIN, CHAT_SIZE, PanelMsg, Runner, Which, panel, physical, place, reveal};
use crate::api::AppState;
use crate::node::now_ms;
use crate::overlay::Msg;
use crate::overlay::displays;
use crate::protocol::live::{LiveBody, to_ratio};
use crate::team_text::{self, Input};
use crate::votes::VoteKind;

impl Runner {
    pub(super) fn open_text(&mut self) {
        if !self.in_team || !self.settings().is_some_and(|s| s.team_text) {
            return;
        }
        let Some(list) = crate::overlay::current_displays(&self.app) else {
            return;
        };
        // 启动后鼠标还没动过时钩子不知道它在哪：放在主显示器中间。
        let (cx, cy) = crate::input::cursor().unwrap_or_else(|| {
            list.first().map_or((0, 0), |d| {
                (d.rect.x + d.rect.width / 2, d.rect.y + d.rect.height / 2)
            })
        });
        let Some(i) = displays::display_at(&list, cx, cy) else {
            return;
        };
        let d = &list[i];
        self.text_at = Some((
            u8::try_from(i + 1).unwrap_or(u8::MAX),
            to_ratio(f64::from(cx - d.rect.x), f64::from(d.rect.width)),
            to_ratio(f64::from(cy - d.rect.y), f64::from(d.rect.height)),
        ));
        // 聊天框在这块显示器的左下角（和英雄联盟的聊天框一个位置），放不下就挪进显示器里。
        let (w_px, h_px) = physical(d, CHAT_SIZE);
        let (mx, my) = physical(d, CHAT_MARGIN);
        let x = (d.rect.x + mx)
            .min(d.rect.x + d.rect.width - w_px)
            .max(d.rect.x);
        let y = (d.rect.y + d.rect.height - h_px - my).max(d.rect.y);
        let Some(w) = self.window(Which::Text) else {
            return;
        };
        place(&w, (x, y, w_px, h_px));
        reveal(&w);
        let _ = w.set_focus();
        let _ = w.emit_to(w.label(), "text:open", ());
    }

    pub(super) fn close_text(&mut self) {
        self.text_at = None;
        self.hide(Which::Text);
        if let Some(state) = self.state() {
            state.voice.send(crate::voice::VoiceMsg::StopPlay);
        }
    }

    /// 输入框交来的一句话。返回 sent、empty、unknown、votesOff、busy。
    pub(super) fn submit_text(&mut self, raw: &str) -> &'static str {
        let answer = match team_text::parse(raw) {
            Input::Empty => "empty",
            Input::Unknown => "unknown",
            Input::Text(text) => {
                if let (Some((display, x, y)), Some(state)) = (self.text_at, self.state()) {
                    state.history.add(crate::history::NewEntry {
                        member: None,
                        ts_ms: now_ms(),
                        kind: crate::history::Kind::Text,
                        text: text.clone(),
                        voice: None,
                    });
                    state.overlays.send(Msg::Text {
                        from: None,
                        display,
                        x,
                        y,
                        text: text.clone(),
                    });
                    state.node.send_live(LiveBody::Text {
                        display,
                        x,
                        y,
                        text,
                    });
                }
                "sent"
            }
            Input::NoFf => {
                let open_ff = self.tally.view().is_some_and(|o| o.kind == VoteKind::Ff);
                if !self.settings().is_some_and(|s| s.team_votes) {
                    "votesOff"
                } else if open_ff {
                    self.answer(false);
                    "sent"
                } else {
                    "unknown"
                }
            }
            Input::Ff | Input::Ready => {
                let kind = if team_text::parse(raw) == Input::Ff {
                    VoteKind::Ff
                } else {
                    VoteKind::Ready
                };
                match self.start_vote(kind, None) {
                    Ok(()) => "sent",
                    Err(_) if !self.settings().is_some_and(|s| s.team_votes) => "votesOff",
                    Err(_) => "busy",
                }
            }
        };
        // 发完不关，可以接着说；空着按回车就收起（同英雄联盟）
        if answer == "empty" {
            self.close_text();
        }
        answer
    }
}

/// 输入框回车：返回 sent、empty、unknown、votesOff、busy。
#[tauri::command]
pub async fn text_send(app: AppHandle, text: String) -> Result<&'static str, String> {
    let (reply, rx) = oneshot::channel();
    panel(&app)?.send(PanelMsg::TextSubmit(text, reply));
    rx.await
        .map_err(|_| crate::tr!("输入框没有响应", "输入框没有响应").to_string())
}

#[tauri::command]
pub fn text_cancel(state: State<'_, AppState>) {
    state.panel.send(PanelMsg::TextCancel);
}
