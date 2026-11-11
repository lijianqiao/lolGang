// 透明窗口的页面：只画，不算规则。Rust 告诉它开哪个轮盘、高亮哪格、关轮盘、在哪放什么信号或表情、弹什么提示、谁在说话；
// 它在空闲时报告一声，让 Rust 把窗口挪开。
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "../i18n"; // 界面语言（3.3.2）：设 <html lang>、收 lang://changed
import { FALLBACK_TEXTURE, TICK_SOUND, allSoundNames, pingById, soundUrl, textureUrl } from "../shared/pings";
import { bundledSoundUrl, customHash, customUrl, emoteIcon, isBundled, type CustomEmote } from "../shared/emotes";
import { EmoteStage, type EmoteArt } from "./emoteStage";
import { IdleWatch } from "./idleWatch";
import { spawnPing, type PingTag } from "./pingFx";
import { TextStage, type TextSpawn } from "./textFx";
import { SoundBank } from "./soundBank";
import { Wheel } from "./wheel";

/** 和 Rust 的 settings2::OverlaySettings 对应。 */
interface OverlaySettings {
  wheelSizePx: number;
  pingSizePx: number;
  pingDurationS: number;
  /** 0 到 1，静音时是 0。 */
  volume: number;
  tickSound: boolean;
  wheel: string[];
  clickPingId: string;
  emoteWheel: string[];
  clickEmote: string;
  emoteSizePx: number;
  emoteSound: boolean;
  customEmotes: CustomEmote[];
  friendEmotes: boolean;
  textStyle: "spot" | "danmaku";
}

interface EmoteSpawn {
  /** self、preview，或队友公钥的前 12 位：每个人同时只有一个表情。 */
  owner: string;
  emote: string;
  fx: number;
  /** 本人放的（包括预览）。 */
  mine: boolean;
  x: number;
  y: number;
  tag?: PingTag;
}

const pingLayer = document.getElementById("pings") as HTMLElement;
const emoteLayer = document.getElementById("emotes") as HTMLElement;
const textLayer = document.getElementById("texts") as HTMLElement;
const toastLayer = document.getElementById("toasts") as HTMLElement;
const voiceLayer = document.getElementById("voice") as HTMLElement;
const wheelSvg = document.getElementById("wheel") as unknown as SVGSVGElement;
const emoteSvg = document.getElementById("emoteWheel") as unknown as SVGSVGElement;
const stage = new EmoteStage(emoteLayer);
const texts = new TextStage(textLayer);

const sounds = new SoundBank();
let settings: OverlaySettings | null = null;

const iconOf = (id: string): string => textureUrl(pingById(id)?.icon ?? FALLBACK_TEXTURE);
const tick = (): void => {
  if (settings?.tickSound) sounds.play(TICK_SOUND, settings.volume * 0.5);
};
const pingWheel = new Wheel(wheelSvg, "PING", tick);
const emoteWheel = new Wheel(emoteSvg, "EMOTE", tick);
/** 正开着（或最近开过）的那个轮盘，wheel:hot 和 wheel:close 都对它。 */
let wheel = pingWheel;

function apply(s: OverlaySettings): void {
  settings = s;
  pingWheel.setSize(s.wheelSizePx);
  pingWheel.setIcons(s.wheel.map(iconOf));
  pingWheel.setCenterIcon(iconOf(s.clickPingId));
  emoteWheel.setSize(s.wheelSizePx);
  emoteWheel.setIcons(s.emoteWheel.map(emoteIcon));
  emoteWheel.setCenterIcon(emoteIcon(s.clickEmote));
  sounds.forget((key) => key.startsWith("custom:")); // 导入表情的音效可能换了，下次放时重新读
}

/** 表情的样子：自己导入的用动画（有的话）和自己选的效果；队友导入的用传过来的静态图，关了「显示队友的表情」就是问号。 */
function artOf(p: EmoteSpawn, s: OverlaySettings): EmoteArt {
  if (isBundled(p.emote)) return { kind: "bundled", slug: p.emote };
  const hash = customHash(p.emote);
  if (!hash) return { kind: "placeholder" };
  const own = s.customEmotes.find((c) => c.id === p.emote);
  if (own) return { kind: "custom", url: customUrl(hash, own.animated ? "anim" : "webp"), fx: own.fx };
  if (p.mine || !s.friendEmotes) return { kind: "placeholder" };
  return { kind: "custom", url: customUrl(hash, "webp"), fx: p.fx };
}

/** 内置表情都有音效；导入的只有自己的、导入时带了音效才有（音效不传给队友）。 */
function emoteSound(ref: string, s: OverlaySettings): void {
  if (!s.emoteSound) return;
  if (isBundled(ref)) {
    void sounds.playLazy(`emote:${ref}`, bundledSoundUrl(ref), s.volume);
    return;
  }
  const hash = customHash(ref);
  if (hash && s.customEmotes.some((c) => c.id === ref && c.sound)) void sounds.playLazy(`custom:${hash}`, customUrl(hash, "sound"), s.volume);
}

const TOAST_MS = 2600;
function showToast(title: string, body: string): void {
  const t = document.createElement("div");
  t.className = "toast";
  const b = document.createElement("b");
  b.textContent = title;
  t.append(b);
  if (body) {
    const small = document.createElement("small");
    small.textContent = body;
    t.append(small);
  }
  toastLayer.replaceChildren(t);
  setTimeout(() => {
    t.classList.add("out");
    setTimeout(() => t.remove(), 200);
  }, TOAST_MS);
}

void listen<OverlaySettings>("overlay:settings", (e) => apply(e.payload));
void listen<{ x: number; y: number; kind: "ping" | "emote" }>("wheel:open", ({ payload: p }) => {
  wheel = p.kind === "emote" ? emoteWheel : pingWheel;
  wheel.open(p.x, p.y);
});
void listen<{ slot: number | null }>("wheel:hot", (e) => wheel.setHot(e.payload.slot ?? -1));
void listen("wheel:close", () => wheel.close());
void listen<{ id: string; x: number; y: number; tag?: PingTag }>("ping:spawn", ({ payload: p }) => {
  const def = pingById(p.id);
  if (!def || !settings) return;
  spawnPing(pingLayer, def, p.x, p.y, { sizePx: settings.pingSizePx, durationS: settings.pingDurationS, tag: p.tag });
  sounds.play(def.sound, settings.volume);
});
void listen<EmoteSpawn>("emote:spawn", ({ payload: p }) => {
  if (!settings) return;
  stage.spawn(p.owner, artOf(p, settings), p.x, p.y, settings.emoteSizePx, p.tag);
  emoteSound(p.emote, settings);
});
void listen<TextSpawn>("text:spawn", (e) => texts.spawn(e.payload));
void listen<{ title: string; body: string }>("toast:show", (e) => showToast(e.payload.title, e.payload.body));

/** 语音：自己在录（还剩几秒）、哪些队友在说（名字的颜色和信号标签一样）。 */
interface VoiceState {
  me: number | null;
  others: { name: string; color: string }[];
}
function voicePill(text: string, cls: string, color?: string): HTMLElement {
  const p = document.createElement("div");
  p.className = `voice-pill ${cls}`;
  if (color) p.style.setProperty("--tag", color);
  const dot = document.createElement("i");
  const label = document.createElement("span");
  label.textContent = text;
  p.append(dot, label);
  return p;
}
void listen<VoiceState>("voice:state", ({ payload: v }) => {
  const pills = v.others.map((o) => voicePill(o.name, "other", o.color));
  if (v.me !== null) pills.push(voicePill(`正在录音 ${v.me} 秒`, "me"));
  voiceLayer.replaceChildren(...pills);
});

// 信号、表情、文字、提示在各自的层里，轮盘开着时它的 class 有 open。
const idle = new IdleWatch(
  () =>
    !pingWheel.isOpen &&
    !emoteWheel.isOpen &&
    pingLayer.childElementCount === 0 &&
    stage.isEmpty &&
    texts.isEmpty &&
    toastLayer.childElementCount === 0 &&
    voiceLayer.childElementCount === 0,
  (v) => void invoke("overlay_idle", { idle: v }),
);
const changes = new MutationObserver(() => idle.check());
for (const l of [pingLayer, emoteLayer, textLayer, toastLayer, voiceLayer]) changes.observe(l, { childList: true });
for (const w of [wheelSvg, emoteSvg]) changes.observe(w, { attributes: true, attributeFilter: ["class"] });

void invoke<OverlaySettings>("overlay_hello").then((s) => {
  apply(s);
  idle.check();
});
void sounds.load(allSoundNames().map((name) => ({ key: name, url: soundUrl(name) })));
