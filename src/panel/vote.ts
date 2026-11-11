// 投票窗口：投降和踢人在屏幕右边，准备确认在中间。能点、不拿焦点。Rust 推 "vote:state" 就重画。
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "../i18n"; // 界面语言（3.3.2）：设 <html lang>、收 lang://changed
import { runCountdown, voteCard, type VoteDto } from "./shared";
import "./panel.css";

const root = document.getElementById("root") as HTMLElement;
let current: { v: VoteDto; bar: HTMLElement } | null = null;

function render(v: VoteDto | null): void {
  if (!v) {
    root.replaceChildren();
    current = null;
    return;
  }
  const { card, bar } = voteCard(v);
  root.replaceChildren(card);
  const fresh = !current;
  current = { v, bar };
  if (fresh) runCountdown(() => current);
}

void listen<VoteDto | null>("vote:state", (e) => render(e.payload));
void invoke<VoteDto | null>("vote_view").then(render);
