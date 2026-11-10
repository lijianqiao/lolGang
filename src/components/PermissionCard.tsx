// macOS 上读不到键鼠时，控制面板每一页顶部的授权卡片（需求 4.7）：原因、打开系统设置、授权后重开。
// 原因由 Rust 的 input/macos.rs 给：没授权，或者系统说已授权但监听建不起来（多半是更新后要删掉再加回来）。
import { useState } from "react";
import { openAccessibilitySettings, relaunchApp } from "../api";

export function PermissionCard({ reason }: { reason: string }) {
  const [error, setError] = useState<string | null>(null);
  return (
    <section className="card permission">
      <h2>需要「辅助功能」权限</h2>
      <p>{reason}</p>
      <p className="muted">没有开发者签名的程序每次更新后，系统都会把它当成新程序，要重新授权一次。</p>
      {error && <p className="banner error">{error}</p>}
      <div className="actions">
        <button className="primary" onClick={() => void openAccessibilitySettings().catch((e) => setError(String(e)))}>
          打开系统设置
        </button>
        <button className="soft" onClick={() => void relaunchApp()}>
          授权好了，重开 lolPing-rust
        </button>
      </div>
    </section>
  );
}
