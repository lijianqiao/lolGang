// 控制面板（3.0）：左边是分页，右边是当前页：「队伍」「信号」「表情」「语音」「快捷键」「功能」「关于」。
// 数据流：启动时读设置和状态 → 改设置时先更新界面，停手 300 毫秒后把最新的整份设置交给 Rust 存（Rust 修正后的值再回填）；
// 导入、删除表情由 Rust 直接存，返回的设置换上（还有没存的改动时只换表情那几项，改动随后一起存）；
// 信号开关在托盘或暂停快捷键那边变了，Rust 推送过来，这里跟着变。
// 关掉控制面板时网页会被销毁，下次打开重新加载：从托盘「检查更新」打开时地址带 #about，直接到那一页。
import { useCallback, useEffect, useRef, useState } from "react";
import { getStatus, onEnabled, onNavigate, readSettings, setEnabled, writeSettings, type KeyClash, type Settings, type Status } from "./api";
import { AboutPage } from "./components/AboutPage";
import { EmotesPage } from "./components/EmotesPage";
import { FeaturesPage } from "./components/FeaturesPage";
import { Icon, type IconName } from "./components/Icon";
import { PermissionCard } from "./components/PermissionCard";
import { PingsPage } from "./components/PingsPage";
import { ShortcutsPage } from "./components/ShortcutsPage";
import { TeamPage } from "./components/TeamPage";
import { VoicePage } from "./components/VoicePage";
import logo from "./assets/logo.svg";
import { useLang } from "./i18n/react";
import "./app.css";
import { k, setLang, t, type Key } from "./i18n";

type Page = "team" | "pings" | "emotes" | "voice" | "shortcuts" | "features" | "about";
const PAGES: { id: Page; label: Key; icon: IconName }[] = [
  { id: "team", label: k("队伍"), icon: "users" },
  { id: "pings", label: k("信号"), icon: "target" },
  { id: "emotes", label: k("表情"), icon: "smile" },
  { id: "voice", label: k("语音"), icon: "mic" },
  { id: "shortcuts", label: k("快捷键"), icon: "keyboard" },
  { id: "features", label: k("功能"), icon: "toggle" },
  { id: "about", label: k("关于"), icon: "info" },
];

const SAVE_DELAY_MS = 300;

export default function App() {
  useLang(); // 换语言时整个控制面板重画（3.3.2）
  const [settings, setSettings] = useState<Settings | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  /** 最近一次保存时被 Rust 换掉的按键，交给快捷键页说明。 */
  const [clashes, setClashes] = useState<KeyClash[]>([]);
  const [page, setPage] = useState<Page>(() => {
    const hash = window.location.hash.slice(1);
    return PAGES.some((p) => p.id === hash) ? (hash as Page) : "team";
  });
  const [error, setError] = useState<string | null>(null);
  const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  /** 最新的设置（包括还没存的改动），定时保存时存它。 */
  const latest = useRef<Settings | null>(null);

  useEffect(() => {
    readSettings().then(
      (s) => {
        latest.current = s;
        setSettings(s);
      },
      (e) => setError(String(e)),
    );
    getStatus().then(
      (s) => {
        setLang(s.lang);
        setStatus(s);
      },
      (e) => setError(String(e)),
    );
    const off = onEnabled((on) => setStatus((s) => (s ? { ...s, enabled: on } : s)));
    const offNav = onNavigate((p) => {
      if (PAGES.some((x) => x.id === p)) setPage(p as Page);
    });
    return () => {
      void off.then((f) => f());
      void offNav.then((f) => f());
    };
  }, []);

  // 主题：选了浅色或深色就写到 <html data-theme>，跟随系统时去掉，由 CSS 按系统设置切换。
  useEffect(() => {
    const root = document.documentElement;
    if (!settings || settings.theme === "system") delete root.dataset.theme;
    else root.dataset.theme = settings.theme;
  }, [settings?.theme]);

  const change = useCallback((patch: Partial<Settings>) => {
    if (!latest.current) return;
    latest.current = { ...latest.current, ...patch };
    setSettings(latest.current);
    if (saveTimer.current) clearTimeout(saveTimer.current);
    saveTimer.current = setTimeout(() => {
      saveTimer.current = null;
      if (!latest.current) return;
      writeSettings(latest.current).then(
        (saved) => {
          if (saveTimer.current) return; // 又改了，等下一次保存的结果
          latest.current = saved.settings;
          setSettings(saved.settings);
          setClashes(saved.clashes);
          setError(null);
        },
        (e) => setError(t("设置没存上：{e}", { e: String(e) })),
      );
    }, SAVE_DELAY_MS);
  }, []);

  const saved = useCallback((s: Settings) => {
    const pending = saveTimer.current !== null && latest.current;
    latest.current = pending
      ? { ...pending, customEmotes: s.customEmotes, emoteWheel: s.emoteWheel, clickEmote: s.clickEmote }
      : s;
    setSettings(latest.current);
  }, []);

  const toggle = (on: boolean) => {
    setStatus((s) => (s ? { ...s, enabled: on } : s));
    void setEnabled(on);
  };

  return (
    <div className="shell">
      <nav className="side">
        <div className="brand">
          <img src={logo} alt="" width={28} height={28} />
          <span>lolPing</span>
        </div>
        {PAGES.map((p) => (
          <button
            key={p.id}
            className={`nav-item${page === p.id ? " active" : ""}`}
            onClick={() => setPage(p.id)}
            aria-current={page === p.id ? "page" : undefined}
          >
            <Icon name={p.icon} />
            {t(p.label)}
          </button>
        ))}
        {status && (
          <p className={`side-status${status.enabled && !status.inputError ? " on" : ""}`}>
            <span className="dot" />
            {status.inputError ? t("信号不可用") : status.enabled ? t("信号开启中") : t("信号已暂停")}
          </p>
        )}
      </nav>
      <main className="content">
        {error && <p className="banner error">{error}</p>}
        {status?.needsPermission && status.inputError && <PermissionCard reason={status.inputError} />}
        {settings && status ? (
          <>
            {page === "team" && <TeamPage settings={settings} onSettings={change} />}
            {page === "pings" && (
              <PingsPage settings={settings} status={status} onChange={change} onToggle={toggle} />
            )}
            {page === "emotes" && (
              <EmotesPage settings={settings} onChange={change} onSaved={saved} onShortcuts={() => setPage("shortcuts")} />
            )}
            {page === "voice" && <VoicePage settings={settings} onChange={change} onShortcuts={() => setPage("shortcuts")} />}
            {page === "shortcuts" && <ShortcutsPage settings={settings} clashes={clashes} onChange={change} />}
            {page === "features" && <FeaturesPage settings={settings} onChange={change} />}
            {page === "about" && <AboutPage settings={settings} onChange={change} />}
          </>
        ) : (
          !error && <p className="loading">{t("正在读取设置…")}</p>
        )}
      </main>
    </div>
  );
}
