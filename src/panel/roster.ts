// 队伍面板：按住面板键（默认 Tab）时在屏幕中间显示，像英雄联盟的 Tab 记分板。上面是投降和准备确认，
// 中间是队员（头像、名字、怎么连着），队友那一行可以发起踢人；开着的投票也画在这里。
// 能点、不拿焦点。每次显示时取一次队伍信息；做法参考 lolPing src/renderer/panel/roster.ts（MIT，© 2026 Danrui Meng）。
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { k, onLangChange, t, type Key } from "../i18n"; // 界面语言（3.3.2）
import type { Member, TeamView } from "../api";
import { hueOf, initialOf, PRESET_COLORS, PRESET_ICONS } from "../avatars";
import { el } from "../shared/dom";
import { runCountdown, voteCard, type VoteDto } from "./shared";
import "./panel.css";

const root = document.getElementById("root") as HTMLElement;
let team: TeamView | null = null;
let vote: VoteDto | null = null;
let error: string | null = null;
let current: { v: VoteDto; bar: HTMLElement } | null = null;
/** 正在说话的队员（公钥的十六进制），那一行亮起。 */
let speaking = new Set<string>();

const PATH: Record<string, Key> = { lan: k("局域网"), relay: k("中继"), offline: k("离线") };
const STATUS: Record<string, Key> = { online: k("在线"), paused: k("信号暂停"), away: k("离开电脑") };

function portrait(m: Member): HTMLElement {
  const p = el("span", "portrait");
  const a = m.avatar;
  if (a?.kind === "image") {
    const img = el("img", "");
    img.src = a.dataUrl;
    img.alt = "";
    p.append(img);
  } else if (a?.kind === "preset" && PRESET_ICONS[a.icon]) {
    p.textContent = PRESET_ICONS[a.icon];
    p.style.background = PRESET_COLORS[a.color] ?? PRESET_COLORS[0];
  } else {
    p.textContent = initialOf(m.nickname, m.fingerprint);
    p.style.background = `hsl(${hueOf(m.fingerprint)} 45% 32%)`;
  }
  return p;
}

function line(m: Member): string {
  if (m.isMe) return t("这台电脑");
  if (m.path === "offline" || m.path === null) return t("离线");
  return [t(m.status ? STATUS[m.status] : "在线"), t(PATH[m.path])].join(" · ");
}

async function act(p: Promise<unknown>): Promise<void> {
  try {
    await p;
    error = null;
  } catch (e) {
    error = String(e);
  }
  render();
}

function memberRow(m: Member): HTMLElement {
  const off = !m.isMe && (m.path === "offline" || m.path === null);
  const talking = speaking.has(m.memberId) ? " speaking" : "";
  const row = el("div", `member${m.isMe ? " me" : ""}${off ? " off" : ""}${talking}`);
  const who = el("div", "who");
  who.append(el("span", "name", m.nickname || m.fingerprint), el("span", "path", line(m)));
  row.append(portrait(m), who);
  if (!m.isMe) {
    const kick = el("button", "kick", "✕");
    kick.title = t("投票踢出 {name}", { name: m.nickname || m.fingerprint });
    kick.setAttribute("aria-label", kick.title);
    kick.addEventListener("click", () => void act(invoke("vote_start", { kind: "kick", target: m.memberId })));
    row.append(kick);
  }
  return row;
}

function render(): void {
  const head = el("header", "cap");
  const title = el("div", "team-title");
  title.append(el("b", "", team?.team?.title ?? t("队伍")), el("span", "count", `${team?.members.length ?? 0} / 16`));
  const ff = el("button", "lol-btn", t("投降"));
  ff.addEventListener("click", () => void act(invoke("vote_start", { kind: "ff" })));
  const ready = el("button", "lol-btn", t("准备确认"));
  ready.addEventListener("click", () => void act(invoke("vote_start", { kind: "ready" })));
  head.append(ff, title, ready);

  const list = el("div", "members");
  for (const m of team?.members ?? []) list.append(memberRow(m));
  const parts: HTMLElement[] = [head, list];
  if (error) parts.push(el("p", "panel-error", error));
  if (vote) {
    const { card, bar } = voteCard(vote);
    const fresh = !current;
    current = { v: vote, bar };
    parts.push(card);
    if (fresh) runCountdown(() => current);
  } else {
    current = null;
  }
  const board = el("section", "board");
  board.append(...parts);
  root.replaceChildren(board);
}

async function load(): Promise<void> {
  try {
    team = await invoke<TeamView>("team_view");
  } catch (e) {
    error = String(e);
  }
  render();
}

onLangChange(render); // 换了语言就重画（3.3.2）
void listen<string[]>("roster:speaking", (e) => {
  speaking = new Set(e.payload);
  if (team) render();
});
void listen<VoteDto | null>("roster:show", (e) => {
  vote = e.payload;
  error = null;
  void load();
});
void listen<VoteDto | null>("vote:state", (e) => {
  vote = e.payload;
  render();
});
void load();
