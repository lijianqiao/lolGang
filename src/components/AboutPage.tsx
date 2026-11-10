// 「关于」页：版本和更新、lolPing 致谢和许可、Riot 素材声明（需求 4.7、4.9）。
// 更新的状态在 Rust 的 update.rs：打开时取一次，之后跟着 "update://changed" 变。
import { getVersion } from "@tauri-apps/api/app";
import { useEffect, useState } from "react";
import notices from "../../THIRD_PARTY_NOTICES.md?raw";
import { onUpdate, updateCheck, updateInstall, updateView, type Settings, type UpdateView } from "../api";
import { Row, Switch, Tip } from "./controls";

interface Props {
  settings: Settings;
  onChange: (patch: Partial<Settings>) => void;
}

/** 状态那一行字。 */
function phaseLine(u: UpdateView): string {
  switch (u.phase) {
    case "checking":
      return "正在检查…";
    case "upToDate":
      return "已经是最新版本";
    case "available":
      return `有新版本 ${u.version}`;
    case "downloading":
      return u.progress === null ? "正在下载…" : `正在下载 ${Math.round(u.progress * 100)}%`;
    case "installing":
      return "正在安装，装好后自动重启…";
    case "failed":
      return u.error ?? "出错了";
    default:
      return "";
  }
}

function UpdateCard({ settings, onChange }: Props) {
  const [u, setU] = useState<UpdateView | null>(null);
  useEffect(() => {
    void updateView().then(setU);
    const off = onUpdate(setU);
    return () => void off.then((f) => f());
  }, []);
  if (!u) return null;
  const busy = u.phase === "checking" || u.phase === "downloading" || u.phase === "installing";
  return (
    <section className="card">
      <h2>更新</h2>
      <div className="row">
        <div className="row-text">
          <span>当前版本 {u.current}</span>
          {phaseLine(u) && <small className={`hint${u.phase === "failed" ? " warn" : ""}`}>{phaseLine(u)}</small>}
        </div>
        <div className="row-control">
          {u.phase === "available" ? (
            <button className="primary" disabled={busy} onClick={() => void updateInstall().catch(() => undefined)}>
              更新并重启
            </button>
          ) : (
            <button className="soft" disabled={busy} onClick={() => void updateCheck()}>
              检查更新
            </button>
          )}
        </div>
      </div>
      {u.phase === "downloading" && (
        <progress className="update-progress" value={u.progress ?? undefined} max={1} aria-label="下载进度" />
      )}
      {u.version && u.notes && (u.phase === "available" || u.phase === "downloading") && (
        <pre className="update-notes">{u.notes}</pre>
      )}
      <Row label="自动检查更新" tip="启动后和每 6 小时查一次，查到只提示，你点了才下载安装">
        <Switch label="自动检查更新" checked={settings.autoUpdateCheck} onChange={(autoUpdateCheck) => onChange({ autoUpdateCheck })} />
      </Row>
    </section>
  );
}

export function AboutPage({ settings, onChange }: Props) {
  const [version, setVersion] = useState("");
  useEffect(() => {
    getVersion().then(setVersion, () => setVersion(""));
  }, []);

  return (
    <div className="page">
      <h1>关于</h1>
      <UpdateCard settings={settings} onChange={onChange} />
      <section className="card prose">
        <h2>lolPing-rust {version}</h2>
        <p>和开黑的朋友在桌面上互发英雄联盟信号和表情、说话、投票。免费，不需要服务器。</p>
        <p className="muted">
          Windows 正式支持；macOS 是实验版
          <Tip text="macOS 没有在真机上验证过，要在「系统设置 → 隐私与安全性 → 辅助功能」里允许 lolPing-rust" />
        </p>
      </section>
      <section className="card prose">
        <h2>致谢</h2>
        <p>
          信号轮盘、信号动画、表情、队伍面板、投票、键鼠手势判定，以及 macOS 的键鼠监听，移植自 lolPing（作者 Danrui Meng，MIT 许可）。
        </p>
        <p className="muted">lolPing-rust 不是 lolPing 的官方版本，是独立的项目，和 lolPing 的作者没有关系。</p>
        <details>
          <summary>第三方声明全文</summary>
          <pre className="notices">{notices}</pre>
        </details>
      </section>
      <section className="card prose">
        <h2>素材声明</h2>
        <p>
          轮盘和信号的图标、音效、表情来自英雄联盟，版权归 Riot Games 所有。lolPing-rust 不代表 Riot Games，也没有得到 Riot Games
          的认可。如 Riot Games 提出要求，将删除这些素材。
        </p>
      </section>
    </div>
  );
}
