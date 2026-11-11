// 「关于」页：版本和更新、去 GitHub / Gitee 点 Star（3.3.2）、lolPing 致谢和许可、Riot 素材声明（需求 4.7、4.9）。
// 更新的状态在 Rust 的 update.rs：打开时取一次，之后跟着 "update://changed" 变。3.3.1 起两个按钮：「检查更新」走 GitHub、「国内检查更新」走 Gitee。
import { getVersion } from "@tauri-apps/api/app";
import { useEffect, useState } from "react";
import notices from "../../THIRD_PARTY_NOTICES.md?raw";
import { onUpdate, openProjectPage, updateCheck, updateCheckCn, updateInstall, updateView, type Settings, type UpdateView } from "../api";
import { Row, Switch, Tip } from "./controls";
import { BrandIcon } from "./Icon";
import { t } from "../i18n";

interface Props {
  settings: Settings;
  onChange: (patch: Partial<Settings>) => void;
}

const SOURCE_TEXT = { github: "GitHub", gitee: "Gitee" } as const;

/** 「（来自 GitHub）」：这次实际来自哪个更新源（3.3.1）。 */
function from(u: UpdateView): string {
  return u.source ? t("（来自 {source}）", { source: SOURCE_TEXT[u.source] }) : "";
}

/** 状态那一行字。 */
function phaseLine(u: UpdateView): string {
  switch (u.phase) {
    case "checking":
      return t("正在检查…");
    case "upToDate":
      return t("已经是最新版本{from}", { from: from(u) });
    case "available":
      return t("有新版本 {version}{from}", { version: u.version ?? "", from: from(u) });
    case "downloading":
      return u.progress === null ? t("正在下载…") : t("正在下载 {pct}%", { pct: Math.round(u.progress * 100) });
    case "installing":
      return t("正在安装，装好后自动重启…");
    case "failed":
      return u.error ?? t("出错了");
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
      <h2>{t("更新")}</h2>
      <div className="row">
        <div className="row-text">
          <span>{t("当前版本 {v}", { v: u.current })}</span>
          {phaseLine(u) && <small className={`hint${u.phase === "failed" ? " warn" : ""}`}>{phaseLine(u)}</small>}
        </div>
        <div className="row-control">
          {u.phase === "available" && (
            <button className="primary" disabled={busy} onClick={() => void updateInstall().catch(() => undefined)}>
              {t("更新并重启")}
            </button>
          )}
          <button className="soft" disabled={busy} onClick={() => void updateCheck()}>
            {t("检查更新")}
          </button>
          <button className="soft" disabled={busy} onClick={() => void updateCheckCn()}>
            {t("国内检查更新")}
          </button>
          <Tip text={t("「检查更新」去 GitHub 查，国内网络可能要开代理；「国内检查更新」去 Gitee 查，国内不用代理。查到新版本后，从查到它的那边下载")} />
        </div>
      </div>
      {u.phase === "downloading" && (
        <progress className="update-progress" value={u.progress ?? undefined} max={1} aria-label={t("下载进度")} />
      )}
      {u.version && u.notes && (u.phase === "available" || u.phase === "downloading") && (
        <pre className="update-notes">{u.notes}</pre>
      )}
      <Row label={t("自动检查更新")} tip={t("启动后和每 6 小时查一次：先查 GitHub，连不上再查 Gitee。查到只提示，你点了才下载安装")}>
        <Switch label={t("自动检查更新")} checked={settings.autoUpdateCheck} onChange={(autoUpdateCheck) => onChange({ autoUpdateCheck })} />
      </Row>
    </section>
  );
}

/** 请用户去 GitHub / Gitee 点 Star（3.3.2）。打不开浏览器时把原因写在按钮下面。 */
function StarCard() {
  const [error, setError] = useState("");
  const open = (site: "github" | "gitee") => {
    setError("");
    openProjectPage(site).catch((e: unknown) => setError(String(e)));
  };
  return (
    <section className="card prose">
      <h2>{t("给项目点个 Star")}</h2>
      <p>{t("觉得好用的话，去 GitHub 或 Gitee 给项目点个 Star ⭐，让更多人看到。国内打不开 GitHub 就点 Gitee。")}</p>
      <div className="link-buttons">
        <button className="soft" onClick={() => open("github")}>
          <BrandIcon name="github" />
          GitHub
        </button>
        <button className="soft" onClick={() => open("gitee")}>
          <BrandIcon name="gitee" />
          Gitee
        </button>
      </div>
      {error && <small className="hint warn">{error}</small>}
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
      <h1>{t("关于")}</h1>
      <UpdateCard settings={settings} onChange={onChange} />
      <StarCard />
      <section className="card prose">
        <h2>lolPing {version}</h2>
        <p>{t("和开黑的朋友在桌面上互发英雄联盟信号和表情、说话、投票。免费，不需要服务器。")}</p>
        <p className="muted">
          {t("Windows 正式支持；macOS 是实验版")}
          <Tip text={t("macOS 没有在真机上验证过，要在「系统设置 → 隐私与安全性 → 辅助功能」里允许 lolPing")} />
        </p>
      </section>
      <section className="card prose">
        <h2>{t("致谢")}</h2>
        <p>
          {t("信号轮盘、信号动画、表情、队伍面板、投票、键鼠手势判定，以及 macOS 的键鼠监听，移植自 lolPing（作者 Danrui Meng，MIT 许可）。")}
        </p>
        <p className="muted">
          {t("这个 lolPing 是参考原版用 Rust 重写的独立项目（GitHub 上叫 lolPing-rust），不是原版 lolPing 的官方版本，和原版的作者没有关系，两者也不互通。")}
        </p>
        <details>
          <summary>{t("第三方声明全文")}</summary>
          <pre className="notices">{notices}</pre>
        </details>
      </section>
      <section className="card prose">
        <h2>{t("素材声明")}</h2>
        <p>
          {t(
            "轮盘和信号的图标、音效、表情来自英雄联盟，版权归 Riot Games 所有。lolPing 不代表 Riot Games，也没有得到 Riot Games 的认可。如 Riot Games 提出要求，将删除这些素材。",
          )}
        </p>
      </section>
    </div>
  );
}
