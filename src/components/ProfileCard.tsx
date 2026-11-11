// 「我的资料」卡片：头像和昵称，队友在信号下面、队员列表里看到的就是这些。点「修改」展开编辑：
// 昵称、内置头像（图标 × 底色）、上传图片（压到 800 字节以内）、恢复自动头像。保存后交给 Rust，在当前队伍写一条资料事件。
import { useRef, useState } from "react";
import { profileSet, type AvatarData, type AvatarInput, type Me, type TeamView } from "../api";
import { PRESET_COLORS, PRESET_ICONS } from "../avatars";
import { compressForAvatar } from "../image";
import { Avatar } from "./Avatar";
import { Tip } from "./controls";
import { t } from "../i18n";

const NICKNAME_MAX = 32;

/** 编辑中的头像：没动、自动、内置、新上传的图片。 */
type Draft = { kind: "keep" } | { kind: "auto" } | { kind: "preset"; icon: number; color: number } | { kind: "image"; data: string };

function shown(me: Me, draft: Draft): AvatarData | null {
  switch (draft.kind) {
    case "keep":
      return me.avatar;
    case "auto":
      return null;
    case "preset":
      return draft;
    case "image":
      return { kind: "image", dataUrl: `data:image/webp;base64,${draft.data}` };
  }
}

function toInput(me: Me, draft: Draft): AvatarInput | null {
  switch (draft.kind) {
    case "auto":
      return null;
    case "preset":
    case "image":
      return draft;
    case "keep":
      if (!me.avatar) return null;
      // 已有的图片头像原样交回去（去掉 data: 前缀）
      return me.avatar.kind === "preset" ? me.avatar : { kind: "image", data: me.avatar.dataUrl.split(",")[1] ?? "" };
  }
}

export function ProfileCard({ me, onSaved }: { me: Me; onSaved: (v: TeamView) => void }) {
  const [editing, setEditing] = useState(false);
  const [name, setName] = useState(me.nickname);
  const [draft, setDraft] = useState<Draft>({ kind: "keep" });
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const file = useRef<HTMLInputElement>(null);

  const open = () => {
    setName(me.nickname);
    setDraft({ kind: "keep" });
    setError(null);
    setEditing(true);
  };

  const save = () => {
    setBusy(true);
    profileSet(name, toInput(me, draft)).then(
      (v) => {
        onSaved(v);
        setEditing(false);
      },
      (e) => setError(String(e)),
    ).finally(() => setBusy(false));
  };

  const preset = draft.kind === "preset" ? draft : me.avatar?.kind === "preset" && draft.kind === "keep" ? me.avatar : null;

  if (!editing) {
    return (
      <section className="card profile">
        <Avatar fingerprint={me.fingerprint} name={me.nickname} avatar={me.avatar} size={48} />
        <div className="member-text">
          <span className="member-name">{me.nickname || t("还没起名字")}</span>
          <small className="hint">
            {t("指纹 {fp}", { fp: me.fingerprint })} <Tip text={t("队友用指纹认出你；昵称和头像会出现在你的信号下面")} />
          </small>
        </div>
        <button className="soft small" onClick={open}>
          {t("修改")}
        </button>
      </section>
    );
  }

  return (
    <section className="card profile-edit">
      <h2>{t("我的资料")}</h2>
      <div className="profile-top">
        <Avatar fingerprint={me.fingerprint} name={name} avatar={shown(me, draft)} size={64} />
        <label className="field">
          <span>{t("昵称")}</span>
          <input value={name} maxLength={NICKNAME_MAX} onChange={(e) => setName(e.target.value)} placeholder={t("队友看到的名字")} />
        </label>
      </div>
      <div className="preset-grid">
        {PRESET_ICONS.map((icon, i) => (
          <button
            key={icon}
            className={`preset${preset?.icon === i ? " current" : ""}`}
            style={{ background: PRESET_COLORS[preset?.color ?? 0] }}
            onClick={() => setDraft({ kind: "preset", icon: i, color: preset?.color ?? 0 })}
            aria-label={t("头像 {icon}", { icon })}
          >
            {icon}
          </button>
        ))}
      </div>
      {preset && (
        <div className="color-row" role="radiogroup" aria-label={t("底色")}>
          {PRESET_COLORS.map((c, i) => (
            <button
              key={c}
              className={`swatch${preset.color === i ? " current" : ""}`}
              style={{ background: c }}
              onClick={() => setDraft({ kind: "preset", icon: preset.icon, color: i })}
              aria-label={t("底色 {n}", { n: i + 1 })}
            />
          ))}
        </div>
      )}
      <div className="actions">
        <button className="soft small" onClick={() => file.current?.click()}>
          {t("上传图片")}
        </button>
        <button className="soft small" onClick={() => setDraft({ kind: "auto" })}>
          {t("用自动头像")}
        </button>
        <input
          ref={file}
          type="file"
          accept="image/*"
          hidden
          onChange={(e) => {
            const f = e.target.files?.[0];
            e.target.value = "";
            if (f) compressForAvatar(f).then((c) => setDraft({ kind: "image", data: c.data }), (err) => setError(String(err.message ?? err)));
          }}
        />
      </div>
      {error && <p className="banner error">{error}</p>}
      <div className="actions end">
        <button className="soft" onClick={() => setEditing(false)}>
          {t("取消")}
        </button>
        <button className="primary" disabled={busy} onClick={save}>
          {t("保存")}
        </button>
      </div>
    </section>
  );
}
