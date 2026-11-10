# lolPing-rust

> 参考 [lolPing](https://github.com/darrenprx/lolPing)（作者 Danrui Meng，MIT 许可）用 Rust 重写的版本：信号、表情、组队、队伍面板、投票和打字沿用 lolPing 的做法，另外加了按住说话的语音、带历史记录的聊天框、6 种字幕特效，界面换成英雄联盟客户端的样子。
>
> **这不是 lolPing 的官方版本**：lolPing-rust 是独立的项目，和 lolPing 的作者没有关系，也不互通。软件装好后在开始菜单和窗口上叫 **lolPing**。3.3.0 以前叫 lolGang。

[![最新版本](https://img.shields.io/github/v/release/lijianqiao/lolPing-rust?label=%E6%9C%80%E6%96%B0%E7%89%88%E6%9C%AC)](https://github.com/lijianqiao/lolPing-rust/releases/latest)
[![下载次数](https://img.shields.io/github/downloads/lijianqiao/lolPing-rust/total?label=%E4%B8%8B%E8%BD%BD)](https://github.com/lijianqiao/lolPing-rust/releases)
[![CI](https://github.com/lijianqiao/lolPing-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/lijianqiao/lolPing-rust/actions/workflows/ci.yml)
[![许可](https://img.shields.io/github/license/lijianqiao/lolPing-rust?label=%E8%AE%B8%E5%8F%AF)](LICENSE)

常驻桌面的英雄联盟小玩具：按住 Alt 拖动，在屏幕任何地方发英雄联盟的信号；按住 Ctrl 拖动放表情。和朋友组队后，你发的信号、表情、文字会出现在队友屏幕上同样的位置，按住 V 说话队友直接听到。

不需要服务器：同一个局域网里直接连，不在一起时经公共中继转发，内容加密。Tauri 2（Rust 核心 + React 控制面板）。

## 看看效果

下面都是程序在屏幕上实际画出来的样子（截自透明窗口，背景是后加的）。

**信号**：危险、正在路上、请求协助、敌人消失，一共 12 种，带音效。

![四种信号的动画](.github/readme/pings.gif)

**表情**：44 个英雄联盟表情，也能导入自己的图，队友没有时自动传过去。

![三个表情的动画](.github/readme/emotes.gif)

**打字**：按 Alt + T 打一句话，队友屏幕上冒出来；有金色、霓虹、弹跳、抖动等 6 种字幕特效，也能改成弹幕。

![四种字幕特效](.github/readme/text.gif)

**控制面板**：轮盘上每一格放什么信号、什么表情都能换，按键全部能改。

<p>
  <img src=".github/readme/panel-pings.png" alt="控制面板：信号轮盘" width="49%">
  <img src=".github/readme/panel-emotes.png" alt="控制面板：表情轮盘" width="49%">
</p>

## 下载

到 [Releases](https://github.com/lijianqiao/lolPing-rust/releases/latest) 下载：

| 系统 | 文件 | 状态 |
|---|---|---|
| Windows 10、11 | `lolping_<版本>_x64-setup.exe` | 正式支持 |
| macOS 13 以上（Apple 芯片） | `lolping_<版本>_aarch64.dmg` | 实验版，没在真机上测试过。要在「系统设置 → 隐私与安全性」里允许 lolPing 使用辅助功能和麦克风 |
| 3.2.0 及以前 | `lolgang_<版本>_…` | 那时叫 lolGang；装着的会自动更新成 lolPing，数据都在 |

第一次运行时：

- Windows 可能弹出「Windows 已保护你的电脑」：安装包没有买代码签名证书，点「更多信息」→「仍要运行」即可。
- macOS 版没有经过苹果公证，提示「无法验证开发者」时，到「系统设置 → 隐私与安全性」里点「仍要打开」。

装好后会自己检查新版本，在「关于」页点「更新并重启」即可升级。

**同时装了原版 lolPing 的话**：两个程序名字一样，开始菜单和桌面上的快捷方式都叫 lolPing、是同一个文件，后装的会覆盖先装的。
程序本身、数据、开机启动互不影响，可以同时运行。分不清时看「已安装的应用」里的发布者：这个项目的是 lijianqiao。

## 怎么用

| 做什么 | 默认按键 |
|---|---|
| 信号轮盘：按住拖动，松开放出指向的信号 | Alt + 左键拖动 |
| 表情轮盘 | Ctrl + 左键拖动 |
| 按住说话，松开发给队友 | V |
| 打开或收起聊天框 | Alt + T |
| 按住显示队伍面板（队员、在线情况、投票） | Tab |
| 暂停或恢复全部拦截 | Ctrl + Alt + P |

所有按键都在控制面板的「快捷键」页改。只有这些手势会被拦下，别的点击和按键照常交给下面的程序；聊天框、说话键和队伍面板只在队伍里才拦。

- **组队**：「队伍」页建一个队伍，把队伍码发给朋友，最多 16 人。
- **聊天框**：上面是这个队伍的历史记录（文字、语音、信号、表情），下面是输入框；语音能点着回放。也能输入 /ff 投降、/noff 反对投降、/ready 准备确认。记录只存在自己电脑上，退出队伍时清空自己这边的，解散队伍时所有人的都清空。
- **语音**：点一下 V 照常打出字母，按住才说话，一次最长 30 秒。「语音」页可以换麦克风、调音量、试音。
- **表情**：44 个英雄联盟表情，也能导入自己的图（动图、带音效都行），队友没有时会自动传过去。
- **投票**：投降要 70% 同意，准备确认要所有人接受，踢人要过半。

点控制面板的关闭按钮只会收进托盘；要退出，右键托盘图标选「退出」。

## 网络和隐私

- 局域网：UDP 端口 47480 广播。第一次运行时 Windows 防火墙会询问，请勾选「专用网络」并允许，否则同一局域网的队友找不到你。
- 互联网：不在同一个局域网时经公共 Nostr 中继转发。所有内容都用队伍密钥加密，中继只看得到密文。「队伍」页可以关掉「允许互联网」，只用局域网。
- 不连别的服务器，不收集任何数据。身份密钥存在系统的凭据管理（Windows 凭据管理器、macOS 钥匙串）。
- 程序意外退出时，会在本机的 `%APPDATA%\com.lijia.lolgang\crash.log`（macOS 是 `~/Library/Application Support/com.lijia.lolgang/crash.log`）里留一行：时间、版本、出错的代码位置。不含按键和聊天内容，只留最近 20 条，不会发到任何地方。反馈「程序自己没了」时请把它贴到 issue 里。

## 开发

```bash
pnpm install
pnpm tauri dev                 # 开发运行
cd src-tauri && cargo test     # Rust 测试
pnpm release                   # 本机打 Windows 安装包（不带更新签名，只用来自己试）
```

## 许可和致谢

代码用 [MIT 许可](LICENSE)。信号轮盘、信号动画、表情、队伍面板、投票、键鼠手势判定，以及 macOS 的键鼠监听，移植自 [lolPing](https://github.com/darrenprx/lolPing)（作者 Danrui Meng，MIT 许可），移植来的文件开头注明了来源。其他第三方声明见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

信号图标、音效、表情来自英雄联盟，版权归 Riot Games 所有，不在 MIT 许可范围内。lolPing-rust（软件名 lolPing）不代表 Riot Games，也没有得到 Riot Games 的认可。
