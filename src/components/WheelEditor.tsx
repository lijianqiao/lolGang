// 轮盘编辑：一个缩小的轮盘，点一格选中，再从下面点一个信号放进去；那个信号已经在别的格子里时两格互换，
// 所以轮盘上永远是 8 个不同的信号。做法同 lolPing 的 wheelLayout.placeOnSlot（MIT，© 2026 Danrui Meng）。
import { useState } from "react";
import { previewPing } from "../api";
import { ALL_PINGS, pingById, textureUrl, type PingId } from "../shared/pings";
import { Icon } from "./Icon";
import { k, t } from "../i18n";
import { tj } from "../i18n/react";

/** 把 id 放到第 slot 格；它已经在别的格子里就两格互换。 */
export function placeOnSlot<T>(wheel: readonly T[], id: T, slot: number): T[] {
  const next = [...wheel];
  const from = next.indexOf(id);
  if (from >= 0) [next[from], next[slot]] = [next[slot], next[from]];
  else next[slot] = id;
  return next;
}

/** 格子中心在示意轮盘上的位置（百分比），0 号在正上方，顺时针。 */
export const slotPos = (i: number): { left: string; top: string } => {
  const a = ((-90 + i * 45) * Math.PI) / 180;
  return { left: `${50 + 37 * Math.cos(a)}%`, top: `${50 + 37 * Math.sin(a)}%` };
};

/** 8 个方向，显示时 t(方向)。 */
export const DIRECTIONS = [k("上"), k("右上"), k("右"), k("右下"), k("下"), k("左下"), k("左"), k("左上")];

export function WheelEditor({ wheel, onChange }: { wheel: PingId[]; onChange: (w: PingId[]) => void }) {
  const [slot, setSlot] = useState(0);
  const selected = pingById(wheel[slot]);

  return (
    <div className="wheel-editor">
      <div className="mini-wheel" role="group" aria-label={t("轮盘的 8 格")}>
        <div className="mini-ring" />
        <div className="mini-core">PING</div>
        {wheel.map((id, i) => {
          const def = pingById(id);
          return (
            <button
              key={i}
              className={`mini-slot${i === slot ? " selected" : ""}`}
              style={slotPos(i)}
              onClick={() => setSlot(i)}
              aria-label={t("{dir}：{name}", { dir: t(DIRECTIONS[i]), name: def ? t(def.name) : id })}
              aria-pressed={i === slot}
            >
              {def && <img src={textureUrl(def.icon)} alt="" />}
            </button>
          );
        })}
      </div>
      <div className="wheel-side">
        <p className="wheel-pick">
          <span>
            {tj("向{dir}拖：{name}", { dir: t(DIRECTIONS[slot]), name: <b>{selected && t(selected.name)}</b> })}
          </span>
          {selected && (
            <button className="soft small" onClick={() => void previewPing(selected.id)}>
              <Icon name="play" size={14} />
              {t("预览")}
            </button>
          )}
        </p>
        <p className="muted">{t("换成：")}</p>
        <div className="palette">
          {ALL_PINGS.map((p) => (
            <button
              key={p.id}
              className={`palette-item${p.id === wheel[slot] ? " current" : ""}`}
              onClick={() => onChange(placeOnSlot(wheel, p.id, slot))}
              title={t(p.name)}
            >
              <img src={textureUrl(p.icon)} alt="" />
              <span>{t(p.name)}</span>
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
