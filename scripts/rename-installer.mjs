// 打包后把安装包改成英文文件名：lolping_<版本>_x64-setup.exe。
// Tauri 按产品名给安装包起名（lolPing_<版本>_x64-setup.exe），产品名还要用在开始菜单、安装目录和「应用」列表里，
// 所以不改产品名，只在打包完成后改文件名。由 `pnpm release` 在 `tauri build` 之后自动运行。
import { existsSync, readFileSync, renameSync, rmSync } from "node:fs";
import { join } from "node:path";

const conf = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8"));
const dir = "src-tauri/target/release/bundle/nsis";
const from = join(dir, `${conf.productName}_${conf.version}_x64-setup.exe`);
const to = join(dir, `${conf.mainBinaryName}_${conf.version}_x64-setup.exe`);

if (!existsSync(from)) {
  console.error(`没找到安装包：${from}`);
  process.exit(1);
}
// 产品名 lolPing 和文件名 lolping 只差大小写：Windows 上是同一个文件，不能先删再改，经过一个临时名字改
if (from.toLowerCase() === to.toLowerCase()) {
  const tmp = `${to}.tmp`;
  renameSync(from, tmp);
  renameSync(tmp, to);
} else {
  // 同一版本重新打包时，上一次改好名的文件会被这次的覆盖
  if (existsSync(to)) rmSync(to);
  renameSync(from, to);
}
console.log(`安装包：${to}`);
