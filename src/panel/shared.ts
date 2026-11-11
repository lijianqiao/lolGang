// 队伍面板和投票窗口共用：投票的数据形状、建元素、投票卡片。数据来自 Rust 的 panel.rs，这里只画。
// 投票卡片的样子参考 lolPing src/renderer/panel/vote.ts（MIT，© 2026 Danrui Meng）。
import { invoke } from "@tauri-apps/api/core";
import { el } from "../shared/dom";
import { t } from "../i18n";

/** 和 Rust 的 panel::VoteDto 对应。 */
export interface VoteDto {
  kind: "ff" | "ready" | "kick";
  title: string;
  voters: { name: string; answer: "yes" | "no" | "none"; me: boolean }[];
  endsAtMs: number;
  durationMs: number;
  canAnswer: boolean;
  mine: boolean | null;
  result: string | null;
}

/** 剩下的时间占总时长的比例，1 是刚开始，0 是到点。 */
export const voteLeft = (v: VoteDto, now: number): number => Math.min(1, Math.max(0, (v.endsAtMs - now) / v.durationMs));

const answer = (yes: boolean): void => void invoke("vote_answer", { yes });

/** 一张投票卡片：标题、每个投票人一格、倒计时条、同意和反对（或我的回答、结果）。返回卡片和要走的倒计时条。 */
export function voteCard(v: VoteDto): { card: HTMLElement; bar: HTMLElement } {
  const card = el("section", `vote-card ${v.kind}`);
  card.append(el("h1", "vote-title", v.title));
  const marks = el("div", "vote-marks");
  for (const voter of v.voters) {
    const m = el("span", `mark ${voter.answer}${voter.me ? " me" : ""}`);
    m.title = voter.name;
    if (v.kind === "ready") m.textContent = voter.name;
    marks.append(m);
  }
  const track = el("div", "vote-track");
  const bar = el("div", "vote-bar");
  track.append(bar);
  const foot = el("div", "vote-foot");
  if (v.result) {
    foot.append(el("span", "vote-said", v.result));
  } else if (v.canAnswer) {
    const yes = el("button", "vote-btn yes", v.kind === "ready" ? t("接受") : t("同意"));
    const no = el("button", "vote-btn no", v.kind === "ready" ? t("拒绝") : t("反对"));
    yes.addEventListener("click", () => answer(true));
    no.addEventListener("click", () => answer(false));
    foot.append(yes, no);
  } else {
    foot.append(el("span", "vote-said", v.mine === null ? t("等大家回答") : v.mine ? t("你同意了") : t("你反对了")));
  }
  card.append(marks, track, foot);
  bar.style.transform = `scaleX(${voteLeft(v, Date.now())})`;
  return { card, bar };
}

/** 每帧挪倒计时条，直到换了新的或者有了结果。 */
export function runCountdown(get: () => { v: VoteDto; bar: HTMLElement } | null): void {
  const step = (): void => {
    const cur = get();
    if (!cur || cur.v.result) return;
    cur.bar.style.transform = `scaleX(${voteLeft(cur.v, Date.now())})`;
    requestAnimationFrame(step);
  };
  requestAnimationFrame(step);
}
