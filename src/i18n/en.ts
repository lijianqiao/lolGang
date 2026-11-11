// English UI text (3.3.2). Must have exactly the same names as zh.ts: the type below makes pnpm build fail otherwise.
import type { zh } from "./zh";

export const en: typeof zh = {
  "features.language": "Language",
  "features.language.tip":
    "Follow system: Chinese if the system display language is Chinese, English for every other language. The tray menu and the small windows change too",
  "features.language.system": "Follow system",
};
