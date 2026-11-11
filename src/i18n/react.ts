// React 里跟着语言重画：在组件里调 useLang()，语言变了这个组件（和它下面的）就重画（3.3.2）。
// 单独一个文件：透明窗口、小窗口不用 React，引 index.ts 不会把 React 带进去。
import { useSyncExternalStore } from "react";
import { lang, onLangChange } from ".";

export const useLang = () => useSyncExternalStore(onLangChange, lang);
