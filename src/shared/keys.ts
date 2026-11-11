import { t } from "../i18n";
// 按键：浏览器的 KeyboardEvent.code → Windows 虚拟键码，以及键名的显示文字。
// 移植自 lolPing src/shared/keys.ts（MIT，© 2026 Danrui Meng）。合法性由 Rust 的 settings2::normalize 最终把关。

/** 修饰键位，和 Rust 的 MOD_* 一致。 */
export const MOD = { ctrl: 1, alt: 2, shift: 4, win: 8 } as const;

export interface Hotkey {
  mods: number;
  vk: number;
}

/** 自定义触发键不能用的键：触发键在全系统被拦下，这些键会弄坏打字、提交、切换焦点、移动光标。Esc 用来取消录键。 */
export const UNSAFE_TRIGGER_VKS: readonly number[] = [0x0d, 0x09, 0x08, 0x20, 0x1b, 0x25, 0x26, 0x27, 0x28];

const MODIFIER_CODES = new Set([
  "ShiftLeft", "ShiftRight", "ControlLeft", "ControlRight", "AltLeft", "AltRight", "MetaLeft", "MetaRight", "OSLeft", "OSRight",
]);
export const isModifierCode = (code: string): boolean => MODIFIER_CODES.has(code);

const NAMED_CODES: Record<string, number> = {
  Space: 0x20, Enter: 0x0d, Tab: 0x09, Backspace: 0x08, Escape: 0x1b, Insert: 0x2d, Delete: 0x2e,
  Home: 0x24, End: 0x23, PageUp: 0x21, PageDown: 0x22, ArrowLeft: 0x25, ArrowUp: 0x26,
  ArrowRight: 0x27, ArrowDown: 0x28, CapsLock: 0x14, Pause: 0x13, ScrollLock: 0x91,
  PrintScreen: 0x2c, ContextMenu: 0x5d, Backquote: 0xc0, Minus: 0xbd, Equal: 0xbb,
  BracketLeft: 0xdb, BracketRight: 0xdd, Backslash: 0xdc, Semicolon: 0xba, Quote: 0xde,
  Comma: 0xbc, Period: 0xbe, Slash: 0xbf, NumpadMultiply: 0x6a, NumpadAdd: 0x6b,
  NumpadSubtract: 0x6d, NumpadDecimal: 0x6e, NumpadDivide: 0x6f,
};

/** KeyboardEvent.code → 虚拟键码。修饰键和认不出的键返回 null。 */
export function codeToVk(code: string): number | null {
  let m = /^Key([A-Z])$/.exec(code);
  if (m) return m[1].charCodeAt(0);
  m = /^Digit([0-9])$/.exec(code);
  if (m) return 0x30 + Number(m[1]);
  m = /^Numpad([0-9])$/.exec(code);
  if (m) return 0x60 + Number(m[1]);
  m = /^F([0-9]{1,2})$/.exec(code);
  if (m) {
    const n = Number(m[1]);
    return n >= 1 && n <= 24 ? 0x6f + n : null;
  }
  return NAMED_CODES[code] ?? null;
}

const VK_LABELS: Record<number, string> = {
  0x20: "Space", 0x0d: "Enter", 0x09: "Tab", 0x08: "Backspace", 0x1b: "Esc", 0x2d: "Insert",
  0x2e: "Delete", 0x24: "Home", 0x23: "End", 0x21: "Page Up", 0x22: "Page Down", 0x25: "←",
  0x26: "↑", 0x27: "→", 0x28: "↓", 0x14: "Caps Lock", 0x13: "Pause", 0x91: "Scroll Lock",
  0x2c: "Print Screen", 0x5d: "Menu", 0xc0: "`", 0xbd: "-", 0xbb: "=", 0xdb: "[", 0xdd: "]",
  0xdc: "\\", 0xba: ";", 0xde: "'", 0xbc: ",", 0xbe: ".", 0xbf: "/", 0x6a: "Num *",
  0x6b: "Num +", 0x6d: "Num -", 0x6e: "Num .", 0x6f: "Num /",
};

export function vkLabel(vk: number): string {
  if ((vk >= 0x41 && vk <= 0x5a) || (vk >= 0x30 && vk <= 0x39)) return String.fromCharCode(vk);
  if (vk >= 0x60 && vk <= 0x69) return `Num ${vk - 0x60}`;
  if (vk >= 0x70 && vk <= 0x87) return `F${vk - 0x6f}`;
  return VK_LABELS[vk] ?? t("键 0x{hex}", { hex: vk.toString(16).toUpperCase().padStart(2, "0") });
}

export function hotkeyParts(h: Hotkey): string[] {
  const out: string[] = [];
  if (h.mods & MOD.ctrl) out.push("Ctrl");
  if (h.mods & MOD.alt) out.push("Alt");
  if (h.mods & MOD.shift) out.push("Shift");
  if (h.mods & MOD.win) out.push("Win");
  return [...out, vkLabel(h.vk)];
}

export const eventMods = (e: { ctrlKey: boolean; altKey: boolean; shiftKey: boolean; metaKey: boolean }): number =>
  (e.ctrlKey ? MOD.ctrl : 0) | (e.altKey ? MOD.alt : 0) | (e.shiftKey ? MOD.shift : 0) | (e.metaKey ? MOD.win : 0);
