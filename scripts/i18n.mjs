// 把 src/i18n/en.ts 和代码里的中文句子对齐（3.3.2）：找出所有 t("…")、k("…")、tj("…") 里的中文原句，
// en.ts 里没有的补上（先填中文原句，等着写英文），代码里已经不用的删掉；已经写好的英文原样保留。
// 用法：node scripts/i18n.mjs          对齐 en.ts
//       node scripts/i18n.mjs --check  只检查：有没有句子还没翻成英文（英文里还带中文），有就报出来、退出码 1
import fs from "node:fs";
import path from "node:path";

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1")), "..");
const enFile = path.join(root, "src", "i18n", "en.ts");
const CJK = /[一-鿿　-〿！-～]/;

function walk(dir, out = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, out);
    else if (/\.(ts|tsx)$/.test(p) && !p.startsWith(path.join(root, "src", "i18n"))) out.push(p);
  }
  return out;
}

// 已有的英文："中文": "English",（一行一句，值可以换行写在下一行）
const existing = new Map();
if (fs.existsSync(enFile)) {
  const src = fs.readFileSync(enFile, "utf8");
  for (const m of src.matchAll(/^\s*("(?:[^"\\]|\\.)*"):\s*("(?:[^"\\]|\\.)*"),?\s*$/gm)) existing.set(JSON.parse(m[1]), JSON.parse(m[2]));
}

if (process.argv[2] === "--check") {
  const left = [...existing].filter(([, v]) => CJK.test(v));
  for (const [zh] of left) console.log(`还没翻：${zh}`);
  console.log(left.length ? `${left.length} 句还没翻成英文` : `${existing.size} 句都有英文`);
  process.exit(left.length ? 1 : 0);
}

const groups = []; // [file, keys[]]
const seen = new Set();
for (const f of walk(path.join(root, "src"))) {
  const src = fs.readFileSync(f, "utf8");
  const keys = [];
  for (const m of src.matchAll(/\b(?:t|k|tj)\(\s*("(?:[^"\\]|\\.)*")/g)) {
    const key = JSON.parse(m[1]);
    if (!seen.has(key)) {
      seen.add(key);
      keys.push(key);
    }
  }
  if (keys.length) groups.push([path.relative(path.join(root, "src"), f).replace(/\\/g, "/"), keys]);
}

const lines = [
  "// 英文（3.3.2）：左边是代码里的中文原句（t(\"…\")、k(\"…\") 里的），右边是英文。",
  "// 这个文件由 node scripts/i18n.mjs 对齐：代码里新加的句子补进来（先填中文，等着写英文），不用的删掉；只改右边的英文。",
  "export const en = {",
];
for (const [file, keys] of groups) {
  lines.push(`  // ${file}`);
  for (const key of keys) lines.push(`  ${JSON.stringify(key)}: ${JSON.stringify(existing.get(key) ?? key)},`);
}
lines.push("};", "");
fs.writeFileSync(enFile, lines.join("\n"));
const dropped = [...existing.keys()].filter((key) => !seen.has(key));
const added = [...seen].filter((key) => !existing.has(key));
console.log(`en.ts：${seen.size} 句（新加 ${added.length}，删掉 ${dropped.length}）`);
for (const key of dropped) console.log(`  删掉：${key}`);
