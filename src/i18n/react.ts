// React 里跟着语言重画：在组件里调 useLang()，语言变了这个组件（和它下面的）就重画（3.3.2）。
// 单独一个文件：透明窗口、小窗口不用 React，引 index.ts 不会把 React 带进去。
import { createElement, Fragment, useSyncExternalStore, type ReactNode } from "react";
import { lang, onLangChange, t, type Key } from ".";

export const useLang = () => useSyncExternalStore(onLangChange, lang);

/** 句子中间夹着元素（按键、粗体）时用：tj("按住 {key} 说话", { key: <kbd>V</kbd> })。整句一起翻，英文语序不同也不会拆坏。 */
export function tj(key: Key, parts: Record<string, ReactNode>): ReactNode[] {
  return t(key)
    .split(/(\{\w+\})/)
    .filter((s) => s !== "")
    .map((s, i) => {
      const name = /^\{(\w+)\}$/.exec(s)?.[1];
      return name !== undefined && name in parts ? createElement(Fragment, { key: i }, parts[name]) : s;
    });
}
