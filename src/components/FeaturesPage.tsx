// 「功能」页：开机启动、主题（原来的「外观」页）、语言（3.3.2），队伍里的三个功能（面板、文字、投票）的开关，
// 以及屏幕上的字的样子（冒出还是弹幕、字号、字幕特效，能在屏幕中间预览）。
// 这些功能的按键在「快捷键」页。改动交给 App 统一保存。
import { textPreview, type Language, type Settings, type TextEffect, type Theme } from "../api";
import { Icon } from "./Icon";
import { Row, Slider, Switch } from "./controls";
import { k, t, type Key } from "../i18n";

const EFFECTS: { value: TextEffect; label: Key }[] = [
  { value: "outline", label: k("描边大字") },
  { value: "gold", label: k("金色渐变") },
  { value: "neon", label: k("霓虹发光") },
  { value: "bounce", label: k("逐字弹跳") },
  { value: "typewriter", label: k("打字机") },
  { value: "shake", label: k("抖动强调") },
];

const THEMES: { value: Theme; label: Key }[] = [
  { value: "system", label: k("跟随系统") },
  { value: "dark", label: k("深色") },
  { value: "light", label: k("浅色") },
];

// 「中文」「English」用各自的写法，不跟着界面语言变：看不懂当前语言的人也找得到自己的语言
const LANGUAGES: { value: Language; label: () => string }[] = [
  { value: "system", label: () => t("跟随系统") },
  { value: "zh", label: () => "中文" },
  { value: "en", label: () => "English" },
];

interface Props {
  settings: Settings;
  onChange: (patch: Partial<Settings>) => void;
}

export function FeaturesPage({ settings: s, onChange }: Props) {
  return (
    <div className="page">
      <h1>{t("功能")}</h1>

      <section className="card">
        <h2>{t("通用")}</h2>
        <Row label={t("开机时自动启动")} tip={t("开机后只在托盘里，不弹出控制面板")}>
          <Switch label={t("开机时自动启动")} checked={s.launchAtStartup} onChange={(launchAtStartup) => onChange({ launchAtStartup })} />
        </Row>
        <Row label={t("主题")} tip={t("只管控制面板。透明窗口上的轮盘和信号是英雄联盟的样子，不跟着变")}>
          <div className="choices" role="radiogroup" aria-label={t("主题")}>
            {THEMES.map((th) => (
              <label key={th.value} className={`choice${s.theme === th.value ? " active" : ""}`}>
                <input type="radio" name="theme" checked={s.theme === th.value} onChange={() => onChange({ theme: th.value })} />
                {t(th.label)}
              </label>
            ))}
          </div>
        </Row>
        <Row label={t("语言")} tip={t("跟随系统：系统的显示语言是中文就用中文，其他语言都用 English。托盘菜单和各个小窗口一起变")}>
          <div className="choices" role="radiogroup" aria-label={t("语言")}>
            {LANGUAGES.map((l) => (
              <label key={l.value} className={`choice${s.language === l.value ? " active" : ""}`}>
                <input type="radio" name="language" checked={s.language === l.value} onChange={() => onChange({ language: l.value })} />
                {l.label()}
              </label>
            ))}
          </div>
        </Row>
      </section>

      <section className="card">
        <h2>{t("队伍")}</h2>
        <Row label={t("队伍面板")} tip={t("在队伍里按住面板键看队员、在线情况，也能发起投票。全屏游戏里照常交给游戏")}>
          <Switch label={t("队伍面板")} checked={s.teamPanel} onChange={(teamPanel) => onChange({ teamPanel })} />
        </Row>
        <Row label={t("打字给队友")} tip={t("在队伍里按文字快捷键打开输入框，回车后这句话出现在队友屏幕上同样的位置")}>
          <Switch label={t("打字给队友")} checked={s.teamText} onChange={(teamText) => onChange({ teamText })} />
        </Row>
        {s.teamText && (
          <>
            <Row label={t("队友的话怎么显示")}>
              <select aria-label={t("队友的话怎么显示")} value={s.textStyle} onChange={(e) => onChange({ textStyle: e.target.value as Settings["textStyle"] })}>
                <option value="spot">{t("在说话的位置冒出来")}</option>
                <option value="danmaku">{t("弹幕从屏幕上方飘过")}</option>
              </select>
            </Row>
            <Slider label={t("字号")} value={s.textSizePx} min={16} max={56} step={1} unit={t(" 像素")} onChange={(v) => onChange({ textSizePx: v })} />
            <Row label={t("字幕特效")} tip={t("屏幕上的字的样子，只影响你自己看到的")}>
              <select aria-label={t("字幕特效")} value={s.textEffect} onChange={(e) => onChange({ textEffect: e.target.value as TextEffect })}>
                {EFFECTS.map((f) => (
                  <option key={f.value} value={f.value}>
                    {t(f.label)}
                  </option>
                ))}
              </select>
              <button className="soft small" onClick={() => void textPreview(s.textStyle, s.textSizePx, s.textEffect)}>
                <Icon name="play" size={14} />
                {t("预览")}
              </button>
            </Row>
          </>
        )}
        <Row label={t("投票")} tip={t("投降要 70% 同意，准备确认要所有人接受，踢人要过半。被踢的人的程序会自己退出队伍。关掉后你不算投票人")}>
          <Switch label={t("投票")} checked={s.teamVotes} onChange={(teamVotes) => onChange({ teamVotes })} />
        </Row>
      </section>
    </div>
  );
}
