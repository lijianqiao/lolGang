// 「队伍」页：我的资料、建队或进队、队伍码、队员和他们的状态、退出和解散、网络。
// 数据流：打开时取一次整份 TeamView；Rust 推 "team://changed" 时重取（走局域网还是中继、掉线这些，
// Rust 每 5 秒看一次，变了才推）。所有改动都返回新的 TeamView，直接换上。
import { useCallback, useEffect, useState } from "react";
import {
  onProtocolMismatch,
  onTeamChanged,
  teamCancel,
  teamCreate,
  teamDissolve,
  teamJoin,
  teamLeave,
  teamRename,
  teamView,
  type Member,
  type Relay,
  type Settings,
  type TeamView,
} from "../api";
import { Avatar } from "./Avatar";
import { Row, Switch, Tip } from "./controls";
import { Icon } from "./Icon";
import { ProfileCard } from "./ProfileCard";
import { k, t, type Key } from "../i18n";

const STATUS_TEXT: Record<NonNullable<Member["status"]>, Key> = { online: k("在线"), paused: k("信号暂停"), away: k("离开电脑") };
const PATH_TEXT: Record<NonNullable<Member["path"]>, Key> = { lan: k("局域网"), relay: k("中继"), offline: k("离线") };
const PLATFORM_TEXT: Record<string, string> = { win: "Windows", mac: "macOS", linux: "Linux" };

function memberLine(m: Member): string {
  if (m.isMe) return t("这台电脑");
  if (m.path === "offline" || m.path === null) return t("离线");
  const parts = [t(m.status ? STATUS_TEXT[m.status] : "在线"), t(PATH_TEXT[m.path])];
  if (m.platform && m.platform !== "win") parts.push(PLATFORM_TEXT[m.platform] ?? m.platform);
  return parts.join(" · ");
}

const DOWNLOAD_URL = "https://github.com/lijianqiao/lolPing-rust/releases/latest";
/** 国内下载（3.3.1）：所有者把每一版原样传到 Gitee，国内不用代理。 */
const DOWNLOAD_URL_CN = "https://gitee.com/lijianqiao/lolPing-rust/releases";

/** 「复制邀请」复制的一段话：从哪下载、怎么进队、队伍码。短队伍码（GT3）旧版本不认，所以写明要哪个版本。 */
function inviteText(code: string): string {
  const needs = code.startsWith("GT3") ? t("（要 3.3.0 或更新的版本，旧版本不认这种队伍码）") : "";
  return [
    t("来 lolPing 开黑："),
    t("1. 下载安装{needs}：{url}", { needs, url: DOWNLOAD_URL }),
    t("   国内下载：{url}", { url: DOWNLOAD_URL_CN }),
    t("2. 打开后点「队伍」→「进队」，粘贴下面这串队伍码："),
    code,
  ].join("\n");
}

/** 他的电脑时间和本机差太多时的提示（Rust 给的是差多少分钟）。两个以上的队友都这样，多半是本机的时间不对。 */
function skewLine(min: number, several: boolean): string {
  const abs = Math.abs(min);
  const amount = abs >= 120 ? t("约 {n} 小时", { n: Math.round(abs / 60) }) : t("约 {n} 分钟", { n: abs });
  const line =
    min > 0
      ? t("对方电脑的时间比你的快{amount}，信号和语音互相收不到。请两边都检查系统时间。", { amount })
      : t("对方电脑的时间比你的慢{amount}，信号和语音互相收不到。请两边都检查系统时间。", { amount });
  return several ? t("{line}多半是你这台电脑的时间不对。", { line }) : line;
}

function dotOf(m: Member): "online" | "away" | "paused" | "offline" {
  if (m.isMe) return "online";
  if (m.path === "offline" || m.path === null) return "offline";
  return m.status === "away" ? "away" : m.status === "paused" ? "paused" : "online";
}

interface Props {
  settings: Settings;
  onSettings: (patch: Partial<Settings>) => void;
}

export function TeamPage({ settings, onSettings }: Props) {
  const [view, setView] = useState<TeamView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [mismatch, setMismatch] = useState(false);

  const load = useCallback(() => {
    teamView().then(setView, (e) => setError(String(e)));
  }, []);

  useEffect(() => {
    load();
    const offChanged = onTeamChanged(load);
    const offMismatch = onProtocolMismatch(() => setMismatch(true));
    return () => {
      void offChanged.then((f) => f());
      void offMismatch.then((f) => f());
    };
  }, [load]);

  const run = (p: Promise<TeamView>) => {
    setBusy(true);
    p.then(
      (v) => {
        setView(v);
        setError(null);
      },
      (e) => setError(String(e)),
    ).finally(() => setBusy(false));
  };

  if (!view) return <div className="page">{error ? <p className="banner error">{error}</p> : <p className="loading">{t("正在读取队伍…")}</p>}</div>;

  return (
    <div className="page">
      <h1>{t("队伍")}</h1>
      {error && <p className="banner error">{error}</p>}
      {mismatch && <p className="banner warn">{t("有队友的版本对不上（多半是 1.x），互相收不到，请都更新到最新版")}</p>}

      <ProfileCard me={view.me} onSaved={setView} />

      {!view.team && <NoTeam busy={busy} onCreate={(t) => run(teamCreate(t))} onJoin={(c) => run(teamJoin(c))} />}
      {view.team?.status === "joining" && (
        <section className="card">
          <h2>
            {t("正在加入队伍…")}
            <Tip text={t("要等至少一位队友在线，同步完队伍信息才算进队。队友在同一个局域网里，或者大家都开着「允许互联网」就行")} />
          </h2>
          <div className="actions">
            <button className="soft" disabled={busy} onClick={() => run(teamCancel())}>
              {t("取消加入")}
            </button>
          </div>
        </section>
      )}
      {view.team?.status === "active" && <ActiveTeam view={view} busy={busy} run={run} />}

      <section className="card">
        <h2>{t("网络")}</h2>
        <Row label={t("允许互联网")} tip={t("不在同一个局域网时经公共中继转发，内容加密，中继看不到")}>
          <Switch label={t("允许互联网")} checked={settings.allowInternet} onChange={(allowInternet) => onSettings({ allowInternet })} />
        </Row>
        <p className="net-status">
          {settings.allowInternet ? (
            <>
              {t("中继：{ok} / {total} 个已连接", { ok: view.relaysOk, total: view.relaysTotal })}
              {view.relays.length > 0 && <Tip text={relayLines(view.relays)} />}
            </>
          ) : (
            t("只用局域网")
          )}
          {view.lanDegraded && <span className="warn-text">{t("　局域网端口被别的程序占了，只能经中继联系")}</span>}
        </p>
      </section>
    </div>
  );
}

const RELAY_STATE: Record<Relay["state"], Key> = { ok: k("已连接"), connecting: k("连接中"), cooling: k("被限速"), retired: k("不再使用") };

/** 每个中继一行：正在发给它的打 ●，状态、发出的事件有多少从它那里回来了、平均多久回来。 */
function relayLines(relays: Relay[]): string {
  const lines = relays.map((r) => {
    const echo =
      r.echoPct === null
        ? t("还没发过")
        : r.echoMs === null
          ? t("回显 {pct}%", { pct: r.echoPct })
          : t("回显 {pct}%，平均 {ms} 毫秒", { pct: r.echoPct, ms: r.echoMs });
    return t("{dot} {host}：{state}，{echo}", { dot: r.sending ? "●" : "○", host: r.host, state: t(RELAY_STATE[r.state]), echo });
  });
  return [t("● 正在发给它（只发回显最好的 3 个，收照常全听）"), t("回显：发出的事件有多少从中继回到自己这里（最近 50 个）"), ...lines].join("\n");
}

function NoTeam({ busy, onCreate, onJoin }: { busy: boolean; onCreate: (t: string) => void; onJoin: (c: string) => void }) {
  const [title, setTitle] = useState(() => t("开黑"));
  const [code, setCode] = useState("");
  return (
    <div className="split">
      <section className="card">
        <h2>
          {t("建队")}
          <Tip text={t("建好后把队伍码发给朋友，最多 16 人")} />
        </h2>
        <form
          className="inline-form"
          onSubmit={(e) => {
            e.preventDefault();
            onCreate(title);
          }}
        >
          <input value={title} maxLength={64} onChange={(e) => setTitle(e.target.value)} aria-label={t("队名")} placeholder={t("队名")} />
          <button className="primary" disabled={busy || !title.trim()}>
            {t("建队")}
          </button>
        </form>
      </section>
      <section className="card">
        <h2>
          {t("进队")}
          <Tip text={t("队伍码以 GT3 开头（3.2.0 及以前建的队伍是 GT2），向队长要")} />
        </h2>
        <form
          className="inline-form"
          onSubmit={(e) => {
            e.preventDefault();
            onJoin(code);
          }}
        >
          <input value={code} onChange={(e) => setCode(e.target.value)} aria-label={t("队伍码")} placeholder="GT3-…" spellCheck={false} />
          <button className="primary" disabled={busy || !code.trim()}>
            {t("进队")}
          </button>
        </form>
      </section>
    </div>
  );
}

function ActiveTeam({ view, busy, run }: { view: TeamView; busy: boolean; run: (p: Promise<TeamView>) => void }) {
  const team = view.team!;
  const [copied, setCopied] = useState<"invite" | "code" | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<"leave" | "dissolve" | null>(null);

  const copy = (what: "invite" | "code") => {
    void navigator.clipboard.writeText(what === "invite" ? inviteText(team.code) : team.code).then(() => {
      setCopied(what);
      setTimeout(() => setCopied(null), 1500);
    });
  };

  return (
    <section className="card">
      <div className="team-head">
        {renaming === null ? (
          <h2>
            {team.title ?? t("队伍")}
            {team.isOwner && (
              <button className="icon-button small" aria-label={t("改队名")} onClick={() => setRenaming(team.title ?? "")}>
                <Icon name="edit" size={15} />
              </button>
            )}
          </h2>
        ) : (
          <form
            className="inline-form"
            onSubmit={(e) => {
              e.preventDefault();
              run(teamRename(renaming));
              setRenaming(null);
            }}
          >
            <input autoFocus value={renaming} maxLength={64} onChange={(e) => setRenaming(e.target.value)} aria-label={t("新队名")} />
            <button className="primary" disabled={busy || !renaming.trim()}>
              {t("保存")}
            </button>
            <button type="button" className="soft" onClick={() => setRenaming(null)}>
              {t("取消")}
            </button>
          </form>
        )}
        <span className="muted">{t("{n} / 16 人", { n: view.members.length })}</span>
      </div>
      <div className="code-row">
        <code className="team-code">{team.code}</code>
        <button className="soft small" onClick={() => copy("invite")}>
          <Icon name="copy" size={14} />
          {copied === "invite" ? t("已复制") : t("复制邀请")}
        </button>
        <button className="soft small" onClick={() => copy("code")}>
          {copied === "code" ? t("已复制") : t("只复制队伍码")}
        </button>
      </div>
      <ul className="members">
        {view.members.map((m) => (
          <li key={m.memberId} className="member">
            <Avatar fingerprint={m.fingerprint} name={m.nickname} avatar={m.avatar} size={38} dot={dotOf(m)} />
            <div className="member-text">
              <span className="member-name">
                {m.nickname || m.fingerprint}
                {m.isMe && <span className="muted">{t("（我）")}</span>}
                {m.isOwner && <span className="chip">{t("队长")}</span>}
              </span>
              <small className="hint">{memberLine(m)}</small>
              {m.clockSkewMin !== null && (
                <small className="hint warn">
                  {skewLine(m.clockSkewMin, view.members.filter((o) => o.clockSkewMin !== null).length >= 2)}
                </small>
              )}
            </div>
          </li>
        ))}
      </ul>
      <div className="actions">
        {confirm === null ? (
          <>
            <button className="soft" disabled={busy} onClick={() => setConfirm("leave")}>
              {t("退出队伍")}
            </button>
            {team.isOwner && (
              <button className="soft danger" disabled={busy} onClick={() => setConfirm("dissolve")}>
                {t("解散队伍")}
              </button>
            )}
          </>
        ) : (
          <>
            <span>{confirm === "leave" ? t("确定退出？") : t("解散后不能撤销，确定？")}</span>
            <button
              className="primary danger"
              disabled={busy}
              onClick={() => {
                run(confirm === "leave" ? teamLeave() : teamDissolve());
                setConfirm(null);
              }}
            >
              {confirm === "leave" ? t("退出") : t("解散")}
            </button>
            <button className="soft" onClick={() => setConfirm(null)}>
              {t("取消")}
            </button>
          </>
        )}
      </div>
    </section>
  );
}
