// 发版工作流（.github/workflows/release.yml）用的脚本，三个子命令：
//   check <标签>                       三处版本号和标签一致、更新公钥已经填了，否则失败（不打一个没法更新的包）
//   collect <平台> <输出目录>          把这个平台打好的包改成英文文件名放进输出目录，另写一个片段：更新包的文件名和签名
//   manifest <输出目录> <标签> <说明文件>  把各平台的片段合成 latest.json（自动更新读它），片段删掉
//   gitee-check <标签>                 所有者手动传完 Gitee 后跑：Gitee 最新发行版是这个标签、每个文件都在、大小和 GitHub 一样（本机跑，不在工作流里）
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

const REPO = "lijianqiao/lolPing-rust";
const UA = { "User-Agent": "lolping-release-check" };

/** 跟着跳转拿到最终的回应（只读开头就停，不把整个安装包下载下来）。 */
async function headOf(url) {
  let current = url;
  for (let i = 0; i < 8; i++) {
    const r = await fetch(current, { redirect: "manual", headers: UA, signal: AbortSignal.timeout(30000) });
    const loc = r.headers.get("location");
    if (r.status >= 300 && r.status < 400 && loc) {
      await r.body?.cancel();
      current = new URL(loc, current).toString();
      continue;
    }
    const length = Number(r.headers.get("content-length") ?? NaN);
    await r.body?.cancel();
    return { status: r.status, length };
  }
  return { status: 0, length: NaN };
}

/** 所有者手动传完 Gitee 以后跑（3.3.1）：Gitee 最新的发行版是不是这个标签，这个版本的四个文件在 Gitee 上都有、大小和 GitHub 上的一样。
 *  程序的「国内检查更新」读的就是 Gitee 最新发行版里的 latest.json，再按同名文件从 Gitee 下载。
 *  文件名按 collect 的规矩推出来，直接看下载地址，不走 GitHub 的接口（匿名的接口每小时只给 60 次，走代理时常被别人用光）。
 *  查 GitHub 要能连上 GitHub：本机要开代理时用 NODE_USE_ENV_PROXY=1 HTTPS_PROXY=… 运行。出错不直接退出进程，免得 Windows 上的 node 在网络还没关时退出报断言。 */
async function giteeCheck(tag) {
  if (!/^v\d+\.\d+\.\d+$/.test(tag ?? "")) return `用法：node scripts/release.mjs gitee-check v3.3.1`;
  const v = tag.slice(1);
  const files = [`lolping_${v}_x64-setup.exe`, `lolping_${v}_aarch64.dmg`, `lolping_${v}_aarch64.app.tar.gz`, "latest.json"];
  const ge = await fetch(`https://gitee.com/api/v5/repos/${REPO}/releases/latest`, { headers: UA });
  if (!ge.ok) return `问 Gitee 最新发行版失败（${ge.status}）`;
  const latest = await ge.json();
  const problems = [];
  if (latest.tag_name !== tag) problems.push(`Gitee 最新的发行版是 ${latest.tag_name}，不是 ${tag}（程序只看最新的那个）`);
  const geNames = new Set((latest.assets ?? []).map((a) => a.name));
  for (const name of files) {
    const gh = await headOf(`https://github.com/${REPO}/releases/download/${tag}/${name}`);
    if (gh.status !== 200) {
      problems.push(`GitHub 上的 ${name} 取不到（${gh.status}），先确认 GitHub 发版完成、能连上 GitHub`);
      continue;
    }
    if (!geNames.has(name)) {
      problems.push(`Gitee 上没有 ${name}`);
      continue;
    }
    const ge1 = await headOf(`https://gitee.com/${REPO}/releases/download/${tag}/${name}`);
    if (ge1.status !== 200) problems.push(`Gitee 上的 ${name} 下载不了（${ge1.status}）`);
    else if (Number.isFinite(ge1.length) && Number.isFinite(gh.length) && ge1.length !== gh.length)
      problems.push(`Gitee 上的 ${name} 是 ${ge1.length} 字节，GitHub 上是 ${gh.length} 字节`);
    else console.log(`  ${name}：${Number.isFinite(gh.length) ? gh.length + " 字节，" : ""}两边都在`);
  }
  if (problems.length) return `Gitee 上的 ${tag} 有问题：\n- ${problems.join("\n- ")}`;
  console.log(`Gitee 上的 ${tag} 没问题：是最新的发行版，${files.length} 个文件都在、大小和 GitHub 一样`);
  return null;
}

const [cmd, ...args] = process.argv.slice(2);
if (cmd === "check") check(args[0] ?? "");
else if (cmd === "collect") collect(args[0], args[1]);
else if (cmd === "manifest") manifest(args[0], args[1], args[2]);
else if (cmd === "gitee-check") {
  const problem = await giteeCheck(args[0]);
  if (problem) {
    console.error(problem);
    process.exitCode = 1;
  }
}
else fail("用法：node scripts/release.mjs check|collect|manifest|gitee-check …");
