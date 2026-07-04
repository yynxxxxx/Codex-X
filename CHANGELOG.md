# Changelog

All notable changes to Codex-X will be documented here.

## [v0.2.13] - 2026-07-04

- macOS 窗口标题栏改为深色融合样式，避免顶部出现系统白色标题条。
- 会话管理页面移除多余外层玻璃边框，统一为完整内容容器。
- 优化会话列表边界与滚动区域，避免列表像卡在外层框外面。

## [v0.2.12] - 2026-07-04

- Windows MSI 安装器回退为默认简洁样式，移除上一版过重的自定义安装界面图。
- 【会话管理】页面新增会话列表，展示标题、Provider、模型、工作目录、更新时间、归档/需同步状态。
- 会话扫描会从 Codex 本地 SQLite threads 表读取最近会话，方便用户直接判断哪些历史 thread 需要同步修复。

## [v0.2.11] - 2026-07-04

- 调整首次启动自动检查更新体验：不再弹出居中的强提醒窗口。
- 自动检测到新版本时仅显示轻量 toast，并保留概览页顶部“发现新版本”提示条。
- 只有用户在【关于】页面主动点击“检查更新”时，检测到新版本才弹出“现在下载 / 稍后”窗口。

## [v0.2.10] - 2026-07-04

- 修复 Release 发布任务上传 Linux 产物时误把 `deb/`、`rpm/` 目录当作资产上传的问题。
- 发布任务现在只收集 `.dmg` / `.msi` / `.deb` / `.rpm` 文件并统一上传。
- 补齐稳定的三平台 Release 自动发布流程。

## [v0.2.9] - 2026-07-04

- 修复 GitHub Actions Release 发布流程：不再由三平台矩阵并发创建 Release，改为先上传构建产物，再由单独发布任务统一创建/更新 Release。
- 修复 `Resource not accessible by integration` 导致 Release 创建失败的问题。
- Release 仍会从 `CHANGELOG.md` 自动读取当前 tag 的更新日志，并上传 macOS / Windows / Linux 安装包。

## [v0.2.8] - 2026-07-04

- 更新页进一步产品化：去掉资源/仓库调试信息，将“有更新”改为更明显的绿色标签。
- 概览页顶部新增轻量“发现新版本”提示条，可直接打开 Releases 页面。
- 新增【会话管理】页面：检查 Codex 本地 sessions / archived_sessions 与 SQLite threads 是否和当前 Provider 同步。
- 新增一键 Provider Sync / 修复历史会话：写入前备份到 `~/.codex/backups_state/provider-sync/`，并保留最近 5 份备份。

## [v0.2.7] - 2026-07-04

- 简化更新检查页展示。
- “打开下载页”改为打开 GitHub Releases 页面。
- 更新弹窗保持简洁，仅提示版本差异。
- 首次启动自动检查更新并弹窗提醒。

## [v0.2.5] - 2026-07-04

- Windows 版双击启动不再额外弹出终端窗口。
- 改进 Windows MSI 安装器品牌展示与图标。
- About 页面外部链接改为系统默认浏览器打开。
- Release 流程加入 Rust cache，后续构建更快。

## [v0.2.4] - 2026-07-04

- 美化 Windows MSI 安装器横幅与对话图。
- Windows 安装包加入应用图标。

## [v0.2.3] - 2026-07-04

- 修复 macOS DMG 中应用图标缺失问题。
- 补充 macOS 安装说明。
- Release 流程加入基础缓存优化。

## [v0.2.2] - 2026-07-04

- macOS / Linux / Windows 首次三平台 Release。
- Linux 产物改为 `deb` / `rpm`，避免 AppImage 图标问题。

## [v0.2.1] - 2026-07-04

- 首次加入应用图标与 GitHub Release 自动发布。
