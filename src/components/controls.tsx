// 设置页共用的小控件：一行设置、开关、滑条、ⓘ 提示。
// 页面上每项只写名字；要解释的话放进 ⓘ（悬停或用 Tab 移到上面时显示），状态文字（比如「已关」）用 hint。
import { useEffect, useId, useRef, type CSSProperties, type ReactNode } from "react";

const TIP_MAX = 280; // 说明框最宽多少
const TIP_GAP = 8; // 说明框和图标之间
const TIP_EDGE = 8; // 说明框离右边那块页面的边至少多远

// 说明框是 position: fixed（不算进页面的宽度，藏着的时候也不会撑出横向滚动条），所以显示前在这里算位置：
// 默认在图标上面、左边对齐图标；右边放不下就往左挪，挪到刚好放下；上面放不下就放到图标下面，下面也放不下就往上挪进窗口。
// 范围是右边那块能滚的页面（.content），不盖到左边的分页栏上。
function placeTip(tip: HTMLElement): void {
  const body = tip.querySelector<HTMLElement>(".tip-body");
  if (!body) return;
  const area = (tip.closest(".content") ?? document.documentElement).getBoundingClientRect();
  body.style.maxWidth = `${Math.min(TIP_MAX, area.width - 2 * TIP_EDGE)}px`;
  const a = tip.getBoundingClientRect();
  const { width, height } = body.getBoundingClientRect();
  const left = Math.max(area.left + TIP_EDGE, Math.min(a.left - 10, area.right - TIP_EDGE - width));
  const above = a.top - TIP_GAP - height;
  const top = above >= area.top + TIP_EDGE ? above : a.bottom + TIP_GAP;
  body.style.left = `${left}px`;
  // 很高的说明框（中继的逐个状态）上下都放不下时，往上挪进窗口，盖住图标也没关系：说明框不接鼠标
  body.style.top = `${Math.max(area.top + TIP_EDGE, Math.min(top, area.bottom - TIP_EDGE - height))}px`;
}

export function Tip({ text }: { text: string }) {
  const id = useId();
  const ref = useRef<HTMLSpanElement>(null);
  const place = () => ref.current && placeTip(ref.current);
  // 说明框开着时页面滚了（比如用 Tab 移到图标上再滚），跟着图标走
  useEffect(() => {
    const onScroll = () => {
      const tip = ref.current;
      if (tip && (tip.matches(":hover") || tip.querySelector(".tip-btn:focus-visible"))) placeTip(tip);
    };
    window.addEventListener("scroll", onScroll, { capture: true, passive: true });
    return () => window.removeEventListener("scroll", onScroll, { capture: true });
  }, []);
  return (
    <span className="tip" ref={ref} onPointerEnter={place} onFocus={place}>
      <button type="button" className="tip-btn" aria-label="说明" aria-describedby={id}>
        i
      </button>
      <span role="tooltip" id={id} className="tip-body">
        {text}
      </span>
    </span>
  );
}

export function Row({ label, tip, hint, children }: { label: string; tip?: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <div className="row">
      <div className="row-text">
        <span className="row-label">
          {label}
          {tip && <Tip text={tip} />}
        </span>
        {hint && <small className="hint">{hint}</small>}
      </div>
      <div className="row-control">{children}</div>
    </div>
  );
}

export function Switch({ checked, onChange, label }: { checked: boolean; onChange: (on: boolean) => void; label: string }) {
  return <input type="checkbox" role="switch" aria-label={label} checked={checked} onChange={(e) => onChange(e.target.checked)} />;
}

export function Slider(props: { label: string; value: number; min: number; max: number; step: number; unit: string; onChange: (v: number) => void }) {
  const { label, value, min, max, step, unit, onChange } = props;
  // 已拖过的一段画亮（CSS 里的 --fill）
  const fill = { "--fill": `${((value - min) / (max - min)) * 100}%` } as CSSProperties;
  return (
    <Row label={label}>
      <input
        type="range"
        aria-label={label}
        min={min}
        max={max}
        step={step}
        value={value}
        style={fill}
        onChange={(e) => onChange(Number(e.target.value))}
      />
      <output className="value">
        {step < 1 ? value.toFixed(1) : value}
        {unit}
      </output>
    </Row>
  );
}
