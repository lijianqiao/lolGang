// 发版工作流（.github/workflows/release.yml）用的脚本，三个子命令：
//   check <标签>                       三处版本号和标签一致、更新公钥已经填了，否则失败（不打一个没法更新的包）
//   collect <平台> <输出目录>          把这个平台打好的包改成英文文件名放进输出目录，另写一个片段：更新包的文件名和签名
//   manifest <输出目录> <标签> <说明文件>  把各平台的片段合成 latest.json（自动更新读它），片段删掉
// 平台：windows、macos（3.1 起不再出 Linux 的包）。安装包的英文名：lolping_<版本>_x64-setup.exe、lolping_<版本>_aarch64.dmg；
// Tauri 按产品名 lolPing 起名，这里统一成上面的小写文件名。
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const conf = () => JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8"));
const BUNDLE = "src-tauri/target/release/bundle";

function fail(message) {
  console.error(message);
  process.exit(1);
}

/** 目录里唯一一个以 suffix 结尾的文件；没有或不止一个都算错。 */
function only(dir, suffix) {
  const found = existsSync(dir) ? readdirSync(dir).filter((f) => f.endsWith(suffix)) : [];
  if (found.length !== 1) fail(`${dir} 里以 ${suffix} 结尾的文件应该正好一个，实际：${found.join("、") || "没有"}`);
  return join(dir, found[0]);
}

function check(tag) {
  const version = conf().version;
  const cargo = /^version\s*=\s*"([^"]+)"/m.exec(readFileSync("src-tauri/Cargo.toml", "utf8"))?.[1];
  const pkg = JSON.parse(readFileSync("package.json", "utf8")).version;
  if (tag !== `v${version}` || cargo !== version || pkg !== version) {
    fail(`版本号对不上：标签 ${tag}，tauri.conf.json ${version}，Cargo.toml ${cargo}，package.json ${pkg}`);
  }
  if (!conf().plugins?.updater?.pubkey) fail("tauri.conf.json 里 plugins.updater.pubkey 是空的：先填上更新公钥");
  console.log(`版本 ${version}，公钥已填`);
}

const PLATFORMS = {
  windows: (v) => ({
    key: "windows-x86_64",
    update: [only(`${BUNDLE}/nsis`, "-setup.exe"), `lolping_${v}_x64-setup.exe`],
    extra: [],
  }),
  macos: (v) => ({
    key: "darwin-aarch64",
    update: [only(`${BUNDLE}/macos`, ".app.tar.gz"), `lolping_${v}_aarch64.app.tar.gz`],
    extra: [[only(`${BUNDLE}/dmg`, ".dmg"), `lolping_${v}_aarch64.dmg`]],
  }),
};

function collect(platform, out) {
  const plan = PLATFORMS[platform];
  if (!plan) fail(`不认识的平台：${platform}`);
  const version = conf().version;
  const { key, update, extra } = plan(version);
  const [from, name] = update;
  const sig = `${from}.sig`;
  if (!existsSync(sig)) fail(`没有更新签名 ${sig}：打包时要带 TAURI_SIGNING_PRIVATE_KEY 和 createUpdaterArtifacts`);
  mkdirSync(out, { recursive: true });
  copyFileSync(from, join(out, name));
  for (const [src, dst] of extra) copyFileSync(src, join(out, dst));
  const fragment = { key, file: name, signature: readFileSync(sig, "utf8").trim() };
  writeFileSync(join(out, `fragment-${platform}.json`), JSON.stringify(fragment));
  console.log(`${platform}：${[name, ...extra.map((e) => e[1])].join("、")}`);
}

function manifest(out, tag, notesFile) {
  const repo = process.env.GITHUB_REPOSITORY;
  if (!repo) fail("没有 GITHUB_REPOSITORY");
  const fragments = readdirSync(out).filter((f) => f.startsWith("fragment-") && f.endsWith(".json"));
  if (fragments.length === 0) fail(`${out} 里没有任何平台的片段`);
  const platforms = {};
  for (const f of fragments) {
    const { key, file, signature } = JSON.parse(readFileSync(join(out, f), "utf8"));
    platforms[key] = { signature, url: `https://github.com/${repo}/releases/download/${tag}/${file}` };
    rmSync(join(out, f));
  }
  const latest = {
    version: tag.replace(/^v/, ""),
    notes: readFileSync(notesFile, "utf8").trim(),
    pub_date: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
    platforms,
  };
  writeFileSync(join(out, "latest.json"), JSON.stringify(latest, null, 2));
  console.log(`latest.json：${Object.keys(platforms).join("、")}`);
}

const [cmd, ...args] = process.argv.slice(2);
if (cmd === "check") check(args[0] ?? "");
else if (cmd === "collect") collect(args[0], args[1]);
else if (cmd === "manifest") manifest(args[0], args[1], args[2]);
else fail("用法：node scripts/release.mjs check|collect|manifest …");
