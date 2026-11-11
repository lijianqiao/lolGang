// 界面语言（3.3.2）：中文、English。用哪种由 Rust 定（设置里选的，或跟随系统换算出来的，见 src-tauri/src/i18n.rs）：
// 透明窗口和三个小窗口建窗口时 Rust 塞了 window.__LOLPING_LANG__；控制面板从 status_get 拿到后调 setLang；
// 之后语言变了 Rust 发 "lang://changed"，所有窗口在这里收到。
// 文字：t("名字") 按当前语言从 zh.ts / en.ts 取。en.ts 的类型规定必须和 zh.ts 的名字一模一样，漏一句 pnpm build 就报错。
// 带参数的句子在字典里是小函数：t("名字")(参数)。
import { listen } from "@tauri-apps/api/event";
import { en } from "./en";
import { zh } from "./zh";

export type Lang = "zh" | "en";
type Dict = typeof zh;

const DICTS: Record<Lang, Dict> = { zh, en };
const HTML_LANG: Record<Lang, string> = { zh: "zh-CN", en: "en" };

const injected = (window as { __LOLPING_LANG__?: unknown }).__LOLPING_LANG__;
let current: Lang = injected === "en" ? "en" : "zh";
const listeners = new Set<() => void>();
document.documentElement.lang = HTML_LANG[current];

/** 现在用的语言。 */
export const lang = (): Lang => current;

/** 换语言：<html lang> 跟着改，通知所有订阅的地方重画。 */
export function setLang(next: Lang): void {
  if (next === current || !(next in DICTS)) return;
  current = next;
  document.documentElement.lang = HTML_LANG[next];
  for (const f of listeners) f();
}

/** 语言变了时叫一下 f；返回取消订阅的函数（React 的 useSyncExternalStore 也用它）。 */
export function onLangChange(f: () => void): () => void {
  listeners.add(f);
  return () => listeners.delete(f);
}

/** 按当前语言取一句话。 */
export function t<K extends keyof Dict>(key: K): Dict[K] {
  return DICTS[current][key];
}

void listen<Lang>("lang://changed", (e) => setLang(e.payload));
