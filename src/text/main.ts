// 聊天框（3.0-M4，原来的输入框）：文字快捷键打开、再按一次或 Esc 收起。上面是这个队伍的历史记录
// （文字、语音、信号、表情，最新的在最下面），下面是输入框；回车发送，发完不关。
// 数据流：打开时取一次全部记录（history_list）、队员名字（team_view）和导入表情（settings_read）；
// 之后 Rust 每记一条推 "history:added"，这里接着加；回放语音交给 Rust（history_play / history_stop），
// 放到哪条由 "voice:playing" 告诉这里。说的话和指令由 Rust 的 panel.rs 处理（清理、认指令、发给队伍）。
// 输入部分参考 lolPing src/renderer/text/main.ts（MIT，© 2026 Danrui Meng）。
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { k, onLangChange, t, type Key } from "../i18n"; // 界面语言（3.3.2）
import type { Settings, TeamView } from "../api";
import { el } from "../shared/dom";
import { emoteIcon, emoteName, type CustomEmote } from "../shared/emotes";
import { pingById, textureUrl } from "../shared/pings";
import "./text.css";

const MAX = 120;
/** 剩最后 20 个字时显示计数。 */
const COUNT_FROM = MAX - 20;
const hintText = (): string => t("回车发送，Esc 收起。/ff 投降　/noff 反对投降　/ready 准备确认");
const ANSWERS: Record<string, Key> = {
  unknown: k("不认识这个指令。可以用 /ff、/noff、/ready"),
  votesOff: k("投票关着，在控制面板的「功能」页打开"),
  busy: k("已经有一个投票在进行"),
};
/** 筛选按钮上的字（按 data-filter）。 */
const TAB_TEXT: Record<string, Key> = { all: k("全部"), text: k("文字"), voice: k("语音"), signal: k("信号和表情") };

/** 和 Rust 的 history::EntryDto 对应。 */
interface Entry {
  id: number;
  memberId: string;
  me: boolean;
  color: string;
  ts: number;
  kind: "text" | "voice" | "ping" | "emote";
  text: string;
  voiceMs: number;
}
type Filter = "all" | "text" | "voice" | "signal";

const log = document.getElementById("log") as HTMLOListElement;
const input = document.getElementById("message") as HTMLInputElement;
const count = document.getElementById("count") as HTMLElement;
const hint = document.getElementById("hint") as HTMLElement;
const tabs = [...document.querySelectorAll<HTMLButtonElement>(".tabs button")];

let entries: Entry[] = [];
let names = new Map<string, string>();
let custom: CustomEmote[] = [];
let filter: Filter = "all";
let playing: number | null = null;

const shows = (e: Entry): boolean =>
  filter === "all" || (filter === "signal" ? e.kind === "ping" || e.kind === "emote" : e.kind === filter);

function time(ts: number): string {
  const d = new Date(ts);
  const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  return d.toDateString() === new Date().toDateString() ? hm : `${d.getMonth() + 1}-${d.getDate()} ${hm}`;
}

/** 信号、表情那一行：小图加一句「发了……」。 */
function signal(icon: string, words: string): HTMLElement {
  const s = el("span", "signal");
  const img = el("img", "");
  img.src = icon;
  img.alt = "";
  s.append(img, el("span", "", words));
  return s;
}

function label(b: HTMLButtonElement): void {
  const on = Number(b.dataset.id) === playing;
  b.classList.toggle("playing", on);
  b.setAttribute("aria-label", on ? t("停止，{n} 秒语音", { n: b.dataset.secs ?? "" }) : t("播放 {n} 秒语音", { n: b.dataset.secs ?? "" }));
}

function voiceButton(e: Entry): HTMLButtonElement {
  const secs = Math.max(1, Math.round(e.voiceMs / 1000));
  const b = el("button", "voice");
  b.type = "button";
  b.dataset.id = String(e.id);
  // 像微信语音：越长的条越宽
  b.style.setProperty("--len", `${Math.min(100, 30 + secs * 5)}%`);
  b.dataset.secs = String(secs);
  label(b);
  b.append(el("i", ""), el("span", "", `${secs}″`));
  b.addEventListener("click", () => {
    void (playing === e.id ? invoke("history_stop") : invoke("history_play", { id: e.id }));
  });
  return b;
}

function row(e: Entry): HTMLLIElement {
  // 行的类名带 k- 前缀，免得和里面的按钮（.voice）撞名
  const li = el("li", `entry k-${e.kind}${e.me ? " me" : ""}`);
  const who = el("b", "who", e.me ? t("我") : (names.get(e.memberId) ?? t("队友")));
  who.style.color = e.color;
  const head = el("div", "head");
  head.append(who, el("time", "", time(e.ts)));
  li.append(head);
  if (e.kind === "text") li.append(el("p", "text", e.text));
  else if (e.kind === "voice") li.append(voiceButton(e));
  else if (e.kind === "ping") {
    const def = pingById(e.text);
    li.append(signal(textureUrl(def?.icon ?? "pingwheel_basicpingrender"), t("发了「{name}」", { name: def ? t(def.name) : e.text })));
  } else li.append(signal(emoteIcon(e.text), t("发了表情「{name}」", { name: emoteName(e.text, custom) })));
  return li;
}

function nearBottom(): boolean {
  return log.scrollHeight - log.scrollTop - log.clientHeight < 40;
}

function render(): void {
  const shown = entries.filter(shows);
  log.replaceChildren(...(shown.length ? shown.map(row) : [el("li", "empty", t("还没有记录"))]));
  log.scrollTop = log.scrollHeight;
}

async function reload(): Promise<void> {
  const [list, view, settings] = await Promise.all([
    invoke<Entry[]>("history_list"),
    invoke<TeamView>("team_view"),
    invoke<Settings>("settings_read"),
  ]);
  entries = list;
  names = new Map(view.members.map((m) => [m.memberId, m.nickname || m.fingerprint]));
  custom = settings.customEmotes;
  render();
}

void listen<Entry>("history:added", ({ payload: e }) => {
  entries.push(e);
  if (!shows(e)) return;
  const stick = nearBottom();
  log.querySelector(".empty")?.remove();
  log.append(row(e));
  if (stick || e.me) log.scrollTop = log.scrollHeight;
});

void listen<number | null>("voice:playing", ({ payload }) => {
  playing = payload;
  for (const b of log.querySelectorAll<HTMLButtonElement>("button.voice")) label(b);
});

for (const tab of tabs) {
  tab.addEventListener("click", () => {
    filter = tab.dataset.filter as Filter;
    for (const x of tabs) x.setAttribute("aria-selected", String(x === tab));
    render();
    input.focus();
  });
}

// ---------- 输入 ----------

function showHint(text: string, warn: boolean): void {
  hint.textContent = text;
  hint.classList.toggle("warn", warn);
}

/** 按字算（一个汉字、一个表情都算一个），不按 UTF-16。 */
function clamp(): void {
  const points = [...input.value];
  if (points.length > MAX) input.value = points.slice(0, MAX).join("");
  const used = Math.min(points.length, MAX);
  count.hidden = used < COUNT_FROM;
  count.textContent = `${used}/${MAX}`;
}

/** 按住回车会连发：一次只交一句，不然同一句会发两遍。 */
let sending = false;

async function send(): Promise<void> {
  if (sending) return;
  sending = true;
  try {
    const answer = await invoke<string>("text_send", { text: input.value });
    if (answer === "sent" || answer === "empty") {
      input.value = "";
      clamp();
      showHint(hintText(), false);
      return;
    }
    showHint(ANSWERS[answer] ? t(ANSWERS[answer]) : hintText(), true);
  } catch (e) {
    showHint(String(e), true);
  } finally {
    sending = false;
  }
}

const close = (): void => {
  input.value = "";
  clamp();
  void invoke("text_cancel");
};

// 输入法正在拼字时不截断（会打断拼写），回车是确认候选字。
input.addEventListener("input", (e) => {
  if (!(e as InputEvent).isComposing) clamp();
  showHint(hintText(), false);
});
input.addEventListener("compositionend", clamp);
// 表单从不提交（发送走回车的处理）：内容安全策略不让在 HTML 里写 onsubmit，在这里拦（3.1.1）
input.form?.addEventListener("submit", (e) => e.preventDefault());
input.addEventListener("keydown", (e) => {
  if (e.isComposing) return;
  if (e.key === "Enter") {
    e.preventDefault();
    void send();
  }
});
// Esc 在哪儿按都收起（焦点可能在筛选按钮或语音按钮上）
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !e.isComposing) {
    e.preventDefault();
    close();
  }
});
document.getElementById("close")?.addEventListener("click", close);
// 点了别处（聊天框失去焦点）就收起，免得一直挡在屏幕上。
window.addEventListener("blur", () => void invoke("text_cancel"));

void listen("text:open", () => {
  showHint(hintText(), false);
  void reload();
  requestAnimationFrame(() => input.focus());
});
// 第一次打开时页面还没加载完，"text:open" 已经发过了：加载完时取一次记录，窗口拿到焦点时把光标放进输入框。
window.addEventListener("focus", () => input.focus());
/** 页面上固定的字（text.html 里只有结构）：筛选按钮、收起按钮、输入框的提示。 */
function labels(): void {
  for (const tab of tabs) tab.textContent = t(TAB_TEXT[tab.dataset.filter ?? "all"] ?? "全部");
  document.querySelector(".tabs")?.setAttribute("aria-label", t("只看"));
  document.getElementById("close")?.setAttribute("aria-label", t("收起聊天框"));
  input.placeholder = t("对队友说…");
  input.setAttribute("aria-label", t("对队友说的话"));
}
// 换了语言（3.3.2）：固定的字、记录、没在报错时的提示都重来
onLangChange(() => {
  labels();
  render();
  if (!hint.classList.contains("warn")) showHint(hintText(), false);
});
labels();
showHint(hintText(), false);
clamp();
void reload();
requestAnimationFrame(() => input.focus());
