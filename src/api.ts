// 控制面板和 Rust 通信的唯一入口。数据流：界面调用这里 → Tauri invoke 到 Rust 的 api.rs → 返回结果；
// 信号开关在别处变了（托盘、暂停快捷键）时 Rust 推 "pings://enabled"，界面用 onEnabled 订阅。
// 设置的合法性（范围、触发键、快捷键、轮盘）由 Rust 的 settings2::normalize 把关，这里只做类型声明和转发。
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { CustomEmote } from "./shared/emotes";
import type { Hotkey } from "./shared/keys";
import type { PingId } from "./shared/pings";

export type NamedTrigger = "alt" | "ctrl" | "shift" | "win" | "capslock" | "mouse4" | "mouse5";
export type TriggerKey = NamedTrigger | { vk: number };
export type Theme = "system" | "light" | "dark";
export type TextEffect = "outline" | "gold" | "neon" | "bounce" | "typewriter" | "shake";

/** 和 Rust 的 settings2::Settings 对应。 */
/** 控制面板的位置和大小（物理像素），和 Rust 的 settings2::PanelRect 对应。 */
export interface PanelRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Settings {
  trigger: TriggerKey;
  dragThresholdPx: number;
  clickPing: boolean;
  clickPingId: PingId;
  gameCheck: boolean;
  wheel: PingId[];
  wheelSizePx: number;
  toggleHotkey: Hotkey;
  pingSizePx: number;
  pingDurationS: number;
  volume: number;
  muted: boolean;
  tickSound: boolean;
  theme: Theme;
  /** 不在同一局域网时经公共中继转发。 */
  allowInternet: boolean;
  /** 自己填的中继；空表示用默认列表。 */
  relays: string[];
  /** 按住它拖动开表情轮盘；"off" 表示不用表情。和信号触发键、快捷键的键撞了时 Rust 会改成 "off"。 */
  emoteTrigger: TriggerKey | "off";
  emoteClick: boolean;
  /** 8 格：内置表情的 slug 或导入表情的 "c:" 加哈希。 */
  emoteWheel: string[];
  clickEmote: string;
  emoteSizePx: number;
  emoteSound: boolean;
  customEmotes: CustomEmote[];
  /** 显示队友导入的表情（要传图）。 */
  friendEmotes: boolean;
  /** 队伍文字：快捷键打开输入框。 */
  teamText: boolean;
  textHotkey: Hotkey;
  /** 队友的话：在原处冒出来，或者弹幕。 */
  textStyle: "spot" | "danmaku";
  /** 屏幕上的字多大（CSS 像素）。 */
  textSizePx: number;
  /** 字幕特效。 */
  textEffect: TextEffect;
  teamVotes: boolean;
  /** 按住面板键显示队伍面板。 */
  teamPanel: boolean;
  /** 面板键的虚拟键码。 */
  panelKey: number;
  /** 语音：按住说话键说话，队友的话直接播放。 */
  voice: boolean;
  /** 说话键（按住）的虚拟键码，不带修饰键。 */
  talkKey: number;
  /** 队友语音的音量 0–100。 */
  voiceVolume: number;
  /** 麦克风的设备 id；空表示跟随系统。 */
  mic: string;
  launchAtStartup: boolean;
  /** 启动后和每 6 小时查一次新版本。 */
  autoUpdateCheck: boolean;
  /** 控制面板上次关掉时的位置和大小；只有 Rust 写，页面存设置时带过去的会被忽略。 */
  panelRect: PanelRect | null;
}

export interface Status {
  enabled: boolean;
  /** 键鼠监听没起来的原因；正常时为 null。 */
  inputError: string | null;
  /** macOS 上监听没起来，顶部显示授权卡片。 */
  needsPermission: boolean;
}

export const readSettings = (): Promise<Settings> => invoke("settings_read");
/** 快捷键页的一行。 */
export type ShortcutRow = "trigger" | "emote" | "toggle" | "text" | "talk" | "panel";
/** 要的键被 Rust 换掉了：哪一行、和哪一行撞了（null：这个键本身不能用）。和 Rust 的 settings2::KeyClash 对应。 */
export interface KeyClash {
  key: ShortcutRow;
  with: ShortcutRow | null;
}
/** 存整份设置，返回 Rust 修正后的结果和这次被换掉的按键。 */
export const writeSettings = (settings: Settings): Promise<{ settings: Settings; clashes: KeyClash[] }> =>
  invoke("settings_write", { settings });
export const getStatus = (): Promise<Status> => invoke("status_get");
export const setEnabled = (on: boolean): Promise<void> => invoke("pings_set_enabled", { on });
/** 录按键期间挂起拦截，键都交给控制面板。 */
export const suspendInput = (on: boolean): Promise<void> => invoke("input_suspend", { on });
export const previewPing = (id: PingId): Promise<void> => invoke("ping_preview", { id });
/** macOS：打开「隐私与安全性 → 辅助功能」。 */
export const openAccessibilitySettings = (): Promise<void> => invoke("open_accessibility_settings");
/** 重开程序（授权后监听才建得起来）。 */
export const relaunchApp = (): Promise<void> => invoke("app_relaunch");
export const previewEmote = (emote: string): Promise<void> => invoke("emote_preview", { emote });
/** 导入表情：文件都是 base64，返回存好后的设置。 */
export const importEmote = (args: { still: string; anim: string | null; sound: string | null; name: string; fx: number }): Promise<Settings> =>
  invoke("emote_import", args);
export const removeEmote = (id: string): Promise<Settings> => invoke("emote_remove", { id });

export const onEnabled = (cb: (on: boolean) => void): Promise<UnlistenFn> =>
  listen<boolean>("pings://enabled", (e) => cb(e.payload));

// ---- 队伍（和 Rust 的 team.rs 对应） ----

export type AvatarData = { kind: "preset"; icon: number; color: number } | { kind: "image"; dataUrl: string };
/** 改头像时传给 Rust 的：图片给 Base64（不超过 800 字节）。 */
export type AvatarInput = { kind: "preset"; icon: number; color: number } | { kind: "image"; data: string };

export interface Me {
  memberId: string;
  nickname: string;
  fingerprint: string;
  avatar: AvatarData | null;
}

export interface TeamInfo {
  title: string | null;
  status: "joining" | "active";
  isOwner: boolean;
  code: string;
}

export interface Member {
  memberId: string;
  nickname: string | null;
  fingerprint: string;
  isOwner: boolean;
  isMe: boolean;
  avatar: AvatarData | null;
  /** 自己是 null。 */
  path: "lan" | "relay" | "offline" | null;
  status: "online" | "paused" | "away" | null;
  platform: string | null;
  /** 他的电脑时间和本机差太多（超过 10 分钟），信号和语音互相收不到：差多少分钟，对方快是正数。平常是 null。 */
  clockSkewMin: number | null;
}

export interface TeamView {
  me: Me;
  team: TeamInfo | null;
  members: Member[];
  lanDegraded: boolean;
  relaysOk: number;
  relaysTotal: number;
  relays: Relay[];
}

/** 一个中继的连接状态和回显统计（3.2.0）。 */
export interface Relay {
  host: string;
  state: "ok" | "connecting" | "cooling" | "retired";
  /** 最近 50 个事件的回显成功率；还没发过时是 null。 */
  echoPct: number | null;
  /** 平均回显往返时间（毫秒）。 */
  echoMs: number | null;
  /** 现在的包发给它（只发回显最好的 3 个）。 */
  sending: boolean;
}

export const teamView = (): Promise<TeamView> => invoke("team_view");
export const teamCreate = (title: string): Promise<TeamView> => invoke("team_create", { title });
export const teamJoin = (code: string): Promise<TeamView> => invoke("team_join", { code });
export const teamCancel = (): Promise<TeamView> => invoke("team_cancel");
export const teamLeave = (): Promise<TeamView> => invoke("team_leave");
export const teamDissolve = (): Promise<TeamView> => invoke("team_dissolve");
export const teamRename = (title: string): Promise<TeamView> => invoke("team_rename", { title });
export const profileSet = (nickname: string, avatar: AvatarInput | null): Promise<TeamView> =>
  invoke("profile_set", { nickname, avatar });

/** 队伍有变化（进队、退出、改名、资料、在线状态）。 */
export const onTeamChanged = (cb: () => void): Promise<UnlistenFn> => listen("team://changed", () => cb());
/** 队友用的是别的协议版本（多半是 1.x）。 */
export const onProtocolMismatch = (cb: (remote: number) => void): Promise<UnlistenFn> =>
  listen<number>("team://mismatch", (e) => cb(e.payload));

/** 在主显示器中间按给的样子放一句字（功能页的预览，还没存的选择也能看）。 */
export const textPreview = (style: Settings["textStyle"], size: number, effect: TextEffect): Promise<void> =>
  invoke("text_preview", { style, size, effect });

// ---- 语音（和 Rust 的 voice/ 对应） ----

export interface Mic {
  id: string;
  name: string;
}

export interface VoiceStatus {
  /** 麦克风或扬声器打不开的原因。 */
  error: string | null;
  /** 试音进行到哪。 */
  test: "recording" | "playing" | null;
}

export const voiceView = (): Promise<{ mics: Mic[]; status: VoiceStatus }> => invoke("voice_view");
/** 录 3 秒放给自己听。 */
export const voiceTest = (): Promise<void> => invoke("voice_test");
export const onVoice = (cb: (s: VoiceStatus) => void): Promise<UnlistenFn> =>
  listen<VoiceStatus>("voice://changed", (e) => cb(e.payload));

// ---- 自动更新（和 Rust 的 update.rs 对应） ----

export interface UpdateView {
  current: string;
  phase: "idle" | "checking" | "upToDate" | "available" | "downloading" | "installing" | "failed";
  version: string | null;
  notes: string | null;
  /** 下载进度 0 到 1；不知道总大小时是 null。 */
  progress: number | null;
  error: string | null;
}

export const updateView = (): Promise<UpdateView> => invoke("update_view");
export const updateCheck = (): Promise<UpdateView> => invoke("update_check");
/** 下载、校验、安装，成功后程序会重启。 */
export const updateInstall = (): Promise<void> => invoke("update_install");
export const onUpdate = (cb: (v: UpdateView) => void): Promise<UnlistenFn> =>
  listen<UpdateView>("update://changed", (e) => cb(e.payload));
/** 托盘要求打开某一页（比如「检查更新」打开「关于」）。 */
export const onNavigate = (cb: (page: string) => void): Promise<UnlistenFn> =>
  listen<string>("nav://page", (e) => cb(e.payload));
