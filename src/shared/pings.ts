import { k, type Key } from "../i18n";
// 信号表（画面用：图、声音、颜色），移植自 lolPing src/shared/pings.ts（MIT，© 2026 Danrui Meng）。
// 信号 id 的合法性、默认轮盘在 Rust 的 settings2.rs 里（PING_IDS、DEFAULT_WHEEL），两边的 id 要一致。

export type PingId =
  | "danger" | "push" | "omw" | "allin" | "assist" | "needvision" | "missing" | "enemyvision" | "generic"
  | "bait" | "visioncleared" | "missingflip";

export interface PingDef {
  id: PingId;
  /** 中文名，显示时 t(name)（3.3.2）。 */
  name: Key;
  /** 轮盘格子上的图：public/lol/textures 里的文件名，不带 .png。 */
  icon: string;
  /** 落点上画的图。两张就是「全力进攻」相撞的两半。 */
  art: string[];
  /** public/lol/sounds 里的文件名，不带 .wav。 */
  sound: string;
  /** 光圈和光晕的颜色。 */
  color: string;
}

/** 全部信号，按设置页列出的顺序：先是默认轮盘的 8 个，再是其余的。 */
export const ALL_PINGS: readonly PingDef[] = [
  { id: "danger", name: k("危险"), icon: "pingwheel_retreatrender", art: ["pingwheel_retreatrender"], sound: "SRP_12", color: "#ff3b55" },
  { id: "push", name: k("推进"), icon: "pingwheel_pushrender", art: ["pingwheel_pushrender"], sound: "SRP_9", color: "#35d27a" },
  { id: "omw", name: k("正在路上"), icon: "omw_cyan", art: ["omw_cyan"], sound: "OnMyWay", color: "#2e8bff" },
  { id: "allin", name: k("全力进攻"), icon: "png_all_in", art: ["pingwheel_allin_render_01", "pingwheel_allin_render_02"], sound: "SRP_4", color: "#f2b320" },
  { id: "assist", name: k("请求协助"), icon: "png_assist_me_128x128", art: ["png_assist_me_128x128"], sound: "ComeHere", color: "#35d27a" },
  { id: "needvision", name: k("需要视野"), icon: "mt11_18_vision", art: ["mt11_18_vision"], sound: "SRP_7", color: "#35d27a" },
  { id: "missing", name: k("敌人消失"), icon: "png_enemymissing_128x128", art: ["png_enemymissing_128x128"], sound: "MIA", color: "#f2c518" },
  { id: "enemyvision", name: k("敌方视野"), icon: "pingwheel_enemyvisionrender", art: ["pingwheel_enemyvisionrender"], sound: "AreaIsWarded", color: "#ff3b55" },
  { id: "generic", name: k("普通信号"), icon: "pingwheel_basicpingrender", art: ["pingwheel_basicpingrender"], sound: "Base", color: "#2e8bff" },
  { id: "bait", name: k("诱饵"), icon: "pingwheel_baitrender", art: ["pingwheel_baitrender"], sound: "SRP_11", color: "#efbc12" },
  { id: "visioncleared", name: k("视野已清除"), icon: "pingwheel_visionclearedrender", art: ["pingwheel_visionclearedrender"], sound: "SRP_6", color: "#2b80ff" },
  // 不是英雄联盟的信号：倒过来的「敌人消失」（¿），lolPing #24 加的。
  { id: "missingflip", name: k("倒立的问号"), icon: "enemy_missing_flipped", art: ["enemy_missing_flipped"], sound: "MIA", color: "#f2c518" },
];

export const TICK_SOUND = "button";
export const FALLBACK_TEXTURE = "pingwheel_basicpingrender";

export const pingById = (id: string): PingDef | undefined => ALL_PINGS.find((p) => p.id === id);

export const textureUrl = (name: string): string => `/lol/textures/${name}.png`;
export const soundUrl = (name: string): string => `/lol/sounds/${name}.wav`;

export function allSoundNames(): string[] {
  return [...new Set([...ALL_PINGS.map((p) => p.sound), TICK_SOUND])];
}
