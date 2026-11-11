// 「快捷键」页：所有按键集中在这一页，每一行都能当场录。改动交给 App 统一保存。
// 同一个键不能两用，由 Rust 的 settings2::normalize 把关：撞了它会换掉（文字快捷键换备用、面板键换备用、表情轮盘关掉），
// 保存时一并说出换掉了哪一行、和谁撞了（3.1.1 以前这里又算了一遍撞键规则）。
// 这里记下每一行「刚才要的是什么」，存好后那一行被换了，就在下面说换成了什么、和谁撞了。
import { useEffect, useRef, useState } from "react";
import type { KeyClash, NamedTrigger, Settings, ShortcutRow, TriggerKey } from "../api";
import { hotkeyParts, vkLabel, type Hotkey } from "../shared/keys";
import { Row } from "./controls";
import { KeyCapture } from "./KeyCapture";
import { t } from "../i18n";

export const TRIGGERS: { value: NamedTrigger; label: string }[] = [
  { value: "alt", label: "Alt" },
  { value: "ctrl", label: "Ctrl" },
  { value: "shift", label: "Shift" },
  { value: "win", label: "Win" },
  { value: "capslock", label: "Caps Lock" },
  {
    value: "mouse4",
    get label() {
      return t("鼠标侧键（后退）");
    },
  },
  {
    value: "mouse5",
    get label() {
      return t("鼠标侧键（前进）");
    },
  },
];
const CUSTOM = "custom";
const OFF = "off";

/** 触发键的名字：Alt、鼠标侧键（后退）、自定义键的键名。 */
export function triggerLabel(t: TriggerKey): string {
  return typeof t === "object" ? vkLabel(t.vk) : (TRIGGERS.find((x) => x.value === t)?.label ?? t);
}

type RowId = ShortcutRow;
type Value = TriggerKey | "off" | Hotkey | number;

const NAMES: Record<RowId, string> = {
  get trigger() {
    return t("信号轮盘");
  },
  get emote() {
    return t("表情轮盘");
  },
  get toggle() {
    return t("暂停 / 恢复");
  },
  get text() {
    return t("打字给队友");
  },
  get talk() {
    return t("说话");
  },
  get panel() {
    return t("队伍面板");
  },
};

const valueOf = (s: Settings, id: RowId): Value =>
  ({ trigger: s.trigger, emote: s.emoteTrigger, toggle: s.toggleHotkey, text: s.textHotkey, talk: s.talkKey, panel: s.panelKey })[id];

function label(id: RowId, v: Value): string {
  if (v === OFF) return t("关");
  if (id === "toggle" || id === "text") return hotkeyParts(v as Hotkey).join(" + ");
  if (id === "panel" || id === "talk") return vkLabel(v as number);
  return triggerLabel(v as TriggerKey);
}

/** 要的键被 Rust 换掉了：说换成了什么、和谁撞了（Rust 没说和谁撞，就是这个键本身不能用）。 */
function adjustment(id: RowId, asked: Value, clash: KeyClash, s: Settings): string | null {
  const now = valueOf(s, id);
  if (JSON.stringify(now) === JSON.stringify(asked)) return null;
  const why = clash.with ? t("和「{name}」撞了", { name: NAMES[clash.with] }) : t("{key} 不能用", { key: label(id, asked) });
  return now === OFF ? t("{why}，表情轮盘先关了", { why }) : t("{why}，换成了 {key}", { why, key: label(id, now) });
}

function TriggerPick(props: { value: TriggerKey | "off"; allowOff: boolean; taken?: TriggerKey; onPick: (t: TriggerKey | "off") => void }) {
  const { value, allowOff, taken, onPick } = props;
  const custom = typeof value === "object";
  return (
    <span className="shortcut-controls">
      <select
        aria-label={t("触发键")}
        value={value === OFF ? OFF : custom ? CUSTOM : value}
        onChange={(e) => {
          const v = e.target.value;
          // 自定义先给一个不常用的键，用户再录
          if (v === CUSTOM) onPick({ vk: allowOff ? 0x54 : 0x4a });
          else onPick(v as NamedTrigger | "off");
        }}
      >
        {allowOff && <option value={OFF}>{t("关")}</option>}
        {TRIGGERS.map((tr) => (
          <option key={tr.value} value={tr.value} disabled={tr.value === taken}>
            {tr.label}
            {tr.value === taken ? t("（信号在用）") : ""}
          </option>
        ))}
        <option value={CUSTOM}>{t("自定义按键…")}</option>
      </select>
      {custom && <KeyCapture kind="key" value={value.vk} onPick={(vk) => onPick({ vk })} />}
    </span>
  );
}

interface Props {
  settings: Settings;
  /** 最近一次保存时被换掉的按键。 */
  clashes: KeyClash[];
  onChange: (patch: Partial<Settings>) => void;
}

export function ShortcutsPage({ settings: s, clashes, onChange }: Props) {
  const [asked, setAsked] = useState<Partial<Record<RowId, Value>>>({});
  /** 每一行最近一次要的键为什么被换掉；没被换掉的不在里面。 */
  const [why, setWhy] = useState<Partial<Record<RowId, KeyClash>>>({});
  /** 选了、还没等到保存结果的行。 */
  const waiting = useRef(new Set<RowId>());
  const pick = (id: RowId, value: Value, patch: Partial<Settings>) => {
    setAsked((a) => ({ ...a, [id]: value }));
    waiting.current.add(id);
    onChange(patch);
  };
  // 保存结果回来了：刚选过的那几行按 Rust 说的更新；别的行保留上次的说明
  useEffect(() => {
    if (waiting.current.size === 0) return;
    const rows = [...waiting.current];
    waiting.current.clear();
    setWhy((w) => {
      const next = { ...w };
      for (const id of rows) {
        const c = clashes.find((x) => x.key === id);
        if (c) next[id] = c;
        else delete next[id];
      }
      return next;
    });
  }, [clashes]);
  /** 侧键当触发键时程序要一直看鼠标（3.2.0 起键盘触发只在按住时才看），在那一行说一声。 */
  const sideButton = (t: TriggerKey | "off") => t === "mouse4" || t === "mouse5";
  const SIDE_HINT = t("用侧键触发时程序要一直看鼠标移动，玩游戏时比键盘触发多占一点 CPU");
  const note = (id: RowId) => {
    const a = asked[id];
    const c = why[id];
    const text = a === undefined || !c ? null : adjustment(id, a, c, s);
    return text && <span className="adjusted">{text}</span>;
  };

  return (
    <div className="page">
      <h1>{t("快捷键")}</h1>
      <section className="card">
        <Row
          label={t("信号轮盘（按住拖动）")}
          tip={t("按住这个键用左键拖动，在按下的位置打开信号轮盘。只拦这个手势，别的点击和按键照常")}
          hint={note("trigger") || (sideButton(s.trigger) && SIDE_HINT)}
        >
          <TriggerPick value={s.trigger} allowOff={false} onPick={(t) => pick("trigger", t, { trigger: t as TriggerKey })} />
        </Row>
        <Row
          label={t("表情轮盘（按住拖动）")}
          tip={t("和信号轮盘一样，打开的是表情轮盘。不能和信号轮盘用同一个键")}
          hint={note("emote") || (sideButton(s.emoteTrigger) && SIDE_HINT)}
        >
          <TriggerPick value={s.emoteTrigger} allowOff taken={s.trigger} onPick={(t) => pick("emote", t, { emoteTrigger: t })} />
        </Row>
        <Row label={t("暂停 / 恢复")} tip={t("随时停掉或恢复全部拦截")} hint={note("toggle")}>
          <KeyCapture kind="hotkey" value={s.toggleHotkey} onPick={(h) => pick("toggle", h, { toggleHotkey: h })} />
        </Row>
        <Row
          label={t("打字给队友")}
          tip={t("在队伍里按下打开输入框，回车后这句话出现在队友屏幕上")}
          hint={note("text") || (!s.teamText && t("已关，在「功能」页打开"))}
        >
          <KeyCapture kind="hotkey" value={s.textHotkey} onPick={(h) => pick("text", h, { textHotkey: h })} />
        </Row>
        <Row
          label={t("说话（按住）")}
          tip={t("在队伍里按住说话，松开发给队友。点一下照常打出这个字；按下后马上又按别的键也算打字")}
          hint={note("talk") || (!s.voice && t("已关，在「语音」页打开"))}
        >
          <KeyCapture kind="key" value={s.talkKey} onPick={(vk) => pick("talk", vk, { talkKey: vk })} />
        </Row>
        <Row
          label={t("队伍面板（按住）")}
          tip={t("在队伍里按住显示队员和投票。单独一个键，不带 Ctrl、Alt")}
          hint={note("panel") || (!s.teamPanel && t("已关，在「功能」页打开"))}
        >
          <KeyCapture kind="plain" value={s.panelKey} onPick={(vk) => pick("panel", vk, { panelKey: vk })} />
        </Row>
      </section>
    </div>
  );
}
