//! 投票：发起、收到队友的、回答、结果、投票窗口。规则在 `votes`。见 `panel` 文件头第 1、3 条。

use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::oneshot;

use super::{
    BALLOT_REPEAT, PanelMsg, READY_SIZE, RESULT_SHOW, Runner, VOTE_SIZE, Which, centred, panel,
    physical, place, reveal,
};
use crate::api::AppState;
use crate::crypto::{self, MemberId};
use crate::node::now_ms;
use crate::overlay::Msg;
use crate::protocol::live::LiveBody;
use crate::votes::{Open, Outcome, VoteKind};

/// 一个投票人在窗口上的样子。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoterDto {
    pub name: String,
    /// yes、no、none。
    pub answer: &'static str,
    pub me: bool,
}

/// 投票窗口和面板要画的投票。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoteDto {
    pub kind: VoteKind,
    pub title: String,
    pub voters: Vec<VoterDto>,
    /// 结束时间（Unix 毫秒）和总时长，页面画倒计时用。
    pub ends_at_ms: u64,
    pub duration_ms: u64,
    pub can_answer: bool,
    /// 我的回答。
    pub mine: Option<bool>,
    /// 结果出来了：那一句结果。
    pub result: Option<String>,
}

impl Runner {
    pub(super) fn tick(&mut self) {
        self.settle();
        if let Some(o) = self.tally.view()
            && self.last_ballot.elapsed() >= BALLOT_REPEAT
            && let Some(me) = self.me
            && let Some(yes) = o.ballot_of(&me)
        {
            let id = o.id;
            self.last_ballot = Instant::now();
            self.send_live(LiveBody::Ballot { id, yes });
        }
        if self.result_until.is_some_and(|t| Instant::now() >= t) {
            self.result_until = None;
            self.publish(None);
            self.hide(Which::Vote);
        }
        // 开着的投票每秒刷新一次（倒计时之外的变化都会马上推，这里只是兜底）。
        if self.tally.view().is_some() {
            self.push();
        }
    }

    pub(super) fn start_vote(
        &mut self,
        kind: VoteKind,
        target: Option<MemberId>,
    ) -> Result<(), String> {
        let s = self.settings().ok_or(crate::tr!(
            "程序还没启动好",
            "lolPing hasn't finished starting"
        ))?;
        if !s.team_votes {
            return Err(crate::tr!(
                "投票已经关掉了，在「功能」页打开",
                "Votes are off. Turn them on in Features"
            )
            .into());
        }
        if !self.in_team {
            return Err(crate::tr!("不在队伍里", "Not in a team").into());
        }
        if self.tally.view().is_some() || self.result_until.is_some() {
            return Err(crate::tr!("已经有一个投票在进行", "A vote is already in progress").into());
        }
        let me = self.me().ok_or(crate::tr!(
            "读不到自己的身份",
            "Couldn't read your own identity"
        ))?;
        if kind == VoteKind::Kick {
            match target {
                Some(t) if t == me => {
                    return Err(crate::tr!("不能踢自己", "You can't kick yourself").into());
                }
                Some(t) if self.names.contains_key(&t) => {}
                _ => {
                    return Err(
                        crate::tr!("队伍里没有这个人", "That person isn't in the team").into(),
                    );
                }
            }
        }
        let id = crypto::random_bytes::<4>()
            .map(u32::from_le_bytes)
            .map_err(|e| e.to_string())?;
        let voters = crate::team::voters(&self.app, me);
        self.tally.start(id, kind, me, target, &voters, now_ms());
        self.send_live(LiveBody::Vote {
            id,
            kind: kind.code(),
            target,
        });
        self.last_ballot = Instant::now();
        self.show_vote(kind);
        self.settle();
        self.push();
        Ok(())
    }

    pub(super) fn remote_vote(
        &mut self,
        from: MemberId,
        id: u32,
        kind: VoteKind,
        target: Option<MemberId>,
    ) {
        if !self.in_team || self.tally.view().is_some() || self.result_until.is_some() {
            return;
        }
        if !self.settings().is_some_and(|s| s.team_votes) {
            return;
        }
        let Some(me) = self.me() else { return };
        if from == me || !self.names.contains_key(&from) {
            return;
        }
        if kind == VoteKind::Kick {
            let known = target.is_some_and(|t| t == me || self.names.contains_key(&t));
            if !known || target == Some(from) {
                return;
            }
        }
        let voters = crate::team::voters(&self.app, me);
        self.tally.start(id, kind, from, target, &voters, now_ms());
        self.show_vote(kind);
        self.settle();
        self.push();
    }

    pub(super) fn answer(&mut self, yes: bool) {
        let Some(me) = self.me() else { return };
        let Some(id) = self.tally.view().map(|o| o.id) else {
            return;
        };
        if self.tally.ballot(me, id, yes) {
            self.last_ballot = Instant::now();
            self.send_live(LiveBody::Ballot { id, yes });
            self.settle();
            self.push();
        }
    }

    /// 结果定了：投票窗口画最后一次（带结果），3 秒后收起；结果用提示说一声；踢的是我就退出队伍。
    pub(super) fn settle(&mut self) {
        let last = self.tally.view().cloned();
        let Some(o) = self.tally.check(now_ms()) else {
            return;
        };
        let line = self.result_line(&o);
        if let Some(open) = last {
            let mut dto = self.dto(&open);
            dto.can_answer = false;
            dto.result = Some(line.clone());
            self.publish(Some(dto));
        }
        self.result_until = Some(Instant::now() + RESULT_SHOW);
        if let Some(state) = self.state() {
            state.overlays.send(Msg::Toast {
                title: line,
                body: String::new(),
            });
        }
        if o.passed && o.kind == VoteKind::Kick && o.target.is_some() && o.target == self.me {
            let app = self.app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = crate::team::commands::team_leave(app.clone()).await;
                if let Some(state) = app.try_state::<AppState>() {
                    state.overlays.send(Msg::Toast {
                        title: crate::tr!(
                            "队友投票把你请出了队伍",
                            "Your teammates voted you out of the team"
                        )
                        .into(),
                        body: crate::tr!("可以用队伍码再进来", "You can rejoin with the team code")
                            .into(),
                    });
                }
            });
        }
    }

    fn result_line(&self, o: &Outcome) -> String {
        match o.kind {
            VoteKind::Ff => crate::tr!(
                format!(
                    "投降投票{}（{}/{}）",
                    if o.passed { "通过" } else { "没通过" },
                    o.yes,
                    o.voters
                ),
                format!(
                    "Surrender vote {} ({}/{})",
                    if o.passed { "passed" } else { "failed" },
                    o.yes,
                    o.voters
                )
            ),
            VoteKind::Ready if o.passed => crate::tr!("大家都准备好了", "Everyone is ready").into(),
            VoteKind::Ready => {
                let names: Vec<String> = o
                    .declined
                    .iter()
                    .chain(&o.missing)
                    .map(|m| self.name(m))
                    .collect();
                crate::tr!(
                    format!("还没准备好：{}", names.join("、")),
                    format!("Not ready: {}", names.join(", "))
                )
            }
            VoteKind::Kick => {
                let name = o.target.map(|t| self.name(&t)).unwrap_or_default();
                crate::tr!(
                    format!(
                        "踢出 {name}：{}（{}/{}）",
                        if o.passed { "通过" } else { "没通过" },
                        o.yes,
                        o.voters
                    ),
                    format!(
                        "Kick {name}: {} ({}/{})",
                        if o.passed { "passed" } else { "failed" },
                        o.yes,
                        o.voters
                    )
                )
            }
        }
    }

    fn dto(&self, o: &Open) -> VoteDto {
        let mine = self.me.and_then(|me| o.ballot_of(&me));
        let voter = self.me.is_some_and(|me| o.voters.contains(&me));
        VoteDto {
            kind: o.kind,
            title: match o.kind {
                VoteKind::Ff => crate::tr!("投降投票", "Surrender vote").into(),
                VoteKind::Ready => crate::tr!("准备确认", "Ready check").into(),
                VoteKind::Kick => {
                    let name = o.target.map(|t| self.name(&t)).unwrap_or_default();
                    crate::tr!(format!("踢出 {name}？"), format!("Kick {name}?"))
                }
            },
            voters: o
                .voters
                .iter()
                .map(|v| VoterDto {
                    name: self.name(v),
                    answer: match o.ballot_of(v) {
                        Some(true) => "yes",
                        Some(false) => "no",
                        None => "none",
                    },
                    me: Some(*v) == self.me,
                })
                .collect(),
            ends_at_ms: o.ends_ms,
            duration_ms: o.kind.duration_ms(),
            can_answer: voter && mine.is_none() && now_ms() < o.ends_ms,
            mine,
            result: None,
        }
    }

    /// 开着的投票交给窗口。
    pub(super) fn push(&self) {
        if let Some(o) = self.tally.view() {
            self.publish(Some(self.dto(o)));
        }
    }

    pub(super) fn publish(&self, dto: Option<VoteDto>) {
        *self.shared.lock().expect("投票锁没有中毒") = dto.clone();
        for w in [&self.roster, &self.vote].into_iter().flatten() {
            let _ = w.emit_to(w.label(), "vote:state", &dto);
        }
    }

    /// 准备确认在主显示器中间；投降和踢人在右边、约 40% 高的地方（同英雄联盟）。
    fn show_vote(&mut self, kind: VoteKind) {
        let Some(list) = crate::overlay::current_displays(&self.app) else {
            return;
        };
        let Some(d) = list.first().cloned() else {
            return;
        };
        let Some(w) = self.window(Which::Vote) else {
            return;
        };
        let rect = if kind == VoteKind::Ready {
            centred(&d, READY_SIZE)
        } else {
            let (w_px, h_px) = physical(&d, VOTE_SIZE);
            let gap = (16.0 * d.scale) as i32;
            (
                d.rect.x + d.rect.width - w_px - gap,
                d.rect.y + (f64::from(d.rect.height) * 0.4) as i32,
                w_px,
                h_px,
            )
        };
        place(&w, rect);
        reveal(&w);
    }
}

/// 投票窗口、面板页打开时取一次投票。
#[tauri::command]
pub fn vote_view(state: State<'_, AppState>) -> Option<VoteDto> {
    state.panel.vote.lock().expect("投票锁没有中毒").clone()
}

/// 发起投票：ff、ready，或 kick 加目标的公钥（十六进制）。
#[tauri::command]
pub async fn vote_start(
    app: AppHandle,
    kind: VoteKind,
    target: Option<String>,
) -> Result<(), String> {
    let target = match target {
        Some(hex) => {
            Some(parse_member(&hex).ok_or(crate::tr!("队员编号不对", "Invalid member ID"))?)
        }
        None => None,
    };
    let (reply, rx) = oneshot::channel();
    panel(&app)?.send(PanelMsg::VoteStart {
        kind,
        target,
        reply,
    });
    rx.await
        .map_err(|_| crate::tr!("投票没有响应", "The vote didn't respond").to_string())?
}

#[tauri::command]
pub fn vote_answer(state: State<'_, AppState>, yes: bool) {
    state.panel.send(PanelMsg::Answer(yes));
}

fn parse_member(hex: &str) -> Option<MemberId> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_ids_parse_from_hex() {
        let id = [0xabu8; 32];
        assert_eq!(parse_member(&crypto::hex(&id)), Some(id));
        assert_eq!(parse_member("ab"), None);
        assert_eq!(parse_member(&"zz".repeat(32)), None);
    }
}
