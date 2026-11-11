// 界面语言（3.3.2）：中文、English。用哪种由 Rust 定（设置里选的，或跟随系统换算出来的，见 src-tauri/src/i18n.rs）：
// 透明窗口和三个小窗口建窗口时 Rust 塞了 window.__LOLPING_LANG__；控制面板从 status_get 拿到后调 setLang；
// 之后语言变了 Rust 发 "lang://changed"，所有窗口在这里收到。
//
// 文字的写法：代码里直接写中文原句，t("检查更新")；带变量的用 {名字} 占位，t("当前版本 {v}", { v })。
// 中文原句就是名字：en.ts 把每句中文对应到英文。en.ts 里没有的句子 t() 不收，pnpm build 报错，所以不会漏翻。
// 放在表里、显示时才翻的（分页名、信号名、表情名……）写成 k("队伍")：只是标一下这是一句要翻的话，显示的地方再 t(它)。
// 新加或改了中文句子：跑 node scripts/i18n.mjs 把 en.ts 补齐，再写英文。
import { listen } from "@tauri-apps/api/event";
import { en } from "./en";

export type Lang = "zh" | "en";
/** 一句要翻的话（中文原句）。 */
export type Key = keyof typeof en;
type Vars = Record<string, string | number>;

const HTML_LANG: Record<Lang, string> = { zh: "zh-CN", en: "en" };

const injected = (window as { __LOLPING_LANG__?: unknown }).__LOLPING_LANG__;
let current: Lang = injected === "en" ? "en" : "zh";
const listeners = new Set<() => void>();
document.documentElement.lang = HTML_LANG[current];

/** 现在用的语言。 */
export const lang = (): Lang => current;

/** 换语言：<html lang> 跟着改，通知所有订阅的地方重画。 */
export function setLang(next: Lang): void {
  if (next === current || !(next in HTML_LANG)) return;
  current = next;
  document.documentElement.lang = HTML_LANG[next];
  for (const f of listeners) f();
}

/** 语言变了时叫一下 f；返回取消订阅的函数（React 的 useSyncExternalStore 也用它）。 */
export function onLangChange(f: () => void): () => void {
  listeners.add(f);
  return () => listeners.delete(f);
}

/** 按当前语言取一句话；vars 填进 {名字} 占位。 */
export function t(key: Key, vars?: Vars): string {
  const s: string = current === "zh" ? key : en[key];
  return vars ? s.replace(/\{(\w+)\}/g, (m, name: string) => (name in vars ? String(vars[name]) : m)) : s;
}

/** 标一句放在表里、显示时才翻的话。原样返回，类型保证它在 en.ts 里。 */
export const k = (key: Key): Key => key;

void listen<Lang>("lang://changed", (e) => setLang(e.payload));
