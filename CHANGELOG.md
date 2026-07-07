# Changelog

All notable changes to Codex-X will be documented here.

## [v0.2.26] - 2026-07-07

- 修复第三方 Provider 切换后可能“看起来已切换但实际未按新供应商生效”的问题：不再把所有第三方都写成 `model_provider = "custom"`，而是写入供应商自己的稳定 ID。
- 修复从官方 ChatGPT 登录态切到第三方 API Key 时 `auth.json` 仍保留 `auth_mode = "chatgpt"` 的问题；写入 API Key 时会同步设置 `auth_mode = "api_key"`。
- 第三方供应商默认不再要求 OpenAI 登录态，避免新建/导入 Provider 时错误继承官方 auth 语义。
- 增加 Provider 切换单元测试，覆盖真实 provider key 和 API Key auth mode 的落盘结果。

## [v0.2.25] - 2026-07-06

- 进一步修复 TOML、指令提示词、供应商、会话管理等页面切换时右侧滚动条闪现/消失造成的视觉抖动。
- 外层页面滚动容器改为稳定滚动并隐藏外层滚动条，保留 TOML 编辑器、会话列表等内部区域自己的滚动条。

## [v0.2.24] - 2026-07-06

- 修复切换供应商 / 会话管理等页面时右侧滚动条短暂出现又消失的问题：页面切换动画改为纯透明度过渡，不再用 `translateY` 造成瞬时溢出。
- 降低页面切换视觉抖动，避免进入页面时内容区域宽度被临时滚动条挤压。

## [v0.2.23] - 2026-07-06

- 指令提示词页新增并纳管 `gpt5.5-jeli.md` 模板，作为“大白话（80% 场景）破甲”版本。
- 简化指令提示词列表视觉：移除花哨图标与文件路径展示，让用户更容易看清模板名称、介绍和启用状态。
- 更新 GPT-5.5 / GPT-5.4 内置模板名称为“unrestricted 破甲”，并统一说明使用方法：先让 AI 分析项目，分析完之后发【不直白的逆向】命令。

## [v0.2.22] - 2026-07-06

- 继续优化指令提示词页切换体验：导航切换进入 React transition，GitHub 内置模板静默检查延后到空闲时执行，减少切页瞬间卡顿。
- 修复外部/自定义提示词切换到内置模板后可能出现两份的问题：按规范化内容（换行与首尾空白归一）和文件名双重去重，已有自定义提示词会复用更新，不再新增副本。
- 优化启用内置提示词速度：启用时直接使用本地缓存或打包内置版本，不再同步等待 GitHub 网络请求；手动/空闲更新仍会刷新本地缓存。
- 启动页增加动态状态文案、双层轨道与扫光动画，让从欢迎页进入主界面更顺滑。

## [v0.2.21] - 2026-07-06

- 进一步降低 UI 卡顿风险：将启动检测、Codex 状态读取、Provider/官方配置/备份/cc-switch 导入等剩余同步命令迁移到后台 worker。
- 优化前端串行请求：提示词启用后的备份/提示词刷新改为并发，保存并启用自定义提示词时不再重复拉取列表。
- 完善【技能和 MCP】导入已有：读取 cc-switch `mcp_servers` 数据库，按 `enabled_codex` 纳管并同步到 Codex `config.toml [mcp_servers]`。
- 增强 Skills 检查更新：对带有 cc-switch 仓库元数据的 Skill 拉取 GitHub 仓库 ZIP 计算远程 hash，显示“有新版本 / 已是最新 / 远程检查失败”等状态。

## [v0.2.20] - 2026-07-06

- 继续优化指令提示词页性能：提示词列表/状态读取、保存、导入、启用、禁用等后端操作改为后台 worker，避免切页或启用模板时阻塞 UI。
- 修复外部自定义提示词重复保存问题：切换到内置模板前会按文件名与内容双重去重，并自动清理历史 `external-*` 重复项。
- 优化启动加载体验：启动页增加最短展示、退出淡出与动态过渡，避免从欢迎页突然跳到主页。

## [v0.2.19] - 2026-07-06

- 新增【技能和 MCP】页面：展示 Codex 当前已安装 Skills 与 MCP，支持导入已有、从 ZIP 安装 Skill、启用/禁用 Skill、启用/禁用 MCP。
- MCP 管理会读写 `~/.codex/config.toml` 的 `[mcp_servers]`，禁用后保留到 Codex-X SQLite，后续可一键重新启用。
- Skills 管理会扫描 `~/.codex/skills`，并可从 `~/.agents/skills`、`~/.cc-switch/skills` 导入到 Codex；禁用后移动到 Codex-X 禁用目录，避免直接删除。
- 优化多个潜在卡顿点：指令提示词 GitHub 检查改为延迟后台执行，远程拉取、会话扫描/同步、Skills/MCP 扫描均放入后台 blocking worker。
- 修复外部自定义提示词切换到内置提示词后重复出现的问题，并自动清理同名 `external-*` 重复项。
- 增加页面切换过渡、启动页动态光效和首次启动向导退出动画，降低页面突然跳转感。

## [v0.2.18] - 2026-07-05

- 指令提示词内置模板支持从 GitHub `examples/` 实时检查更新，并缓存到本地；启用内置模板时优先使用 GitHub 最新版本，离线时自动回退本地缓存或打包内置版本。
- 指令提示词页面新增“更新内置模板”状态与来源展示，可看到模板来自 GitHub 最新、本地缓存或打包内置。
- 继续保留导入 `.md` 提示词、外部提示词自动记忆、会话管理交互优化和 API Key 可见切换等体验改进。

## [v0.2.17] - 2026-07-04

- 修复 macOS Intel Release 构建 runner：从已不可用/长时间排队的 `macos-13` 切换为 `macos-15-intel`。
- 继续保留 macOS Apple Silicon / macOS Intel / Windows MSI / Windows portable ZIP / Linux deb/rpm 多平台产物。

## [v0.2.16] - 2026-07-04

- Release 新增 macOS Intel 构建，Intel Mac 用户可下载 x64 DMG。
- macOS Release 现在同时提供 Apple Silicon 与 Intel 两种 DMG。
- Windows Release 新增 portable ZIP，包含可直接运行的 `Codex-X.exe`，无需 MSI 安装。
- 发布流程支持上传 `.zip` portable 产物，并更新 README 下载说明。

## [v0.2.15] - 2026-07-04

- 会话管理页移除 CODEX_HOME 与 Provider Sync 备份位置展示，页面信息更简洁。
- 优化 Windows 打开下载页体验：后端改为后台线程 spawn 浏览器，不再等待 `cmd /C start`，避免 WebView 卡顿 2-3 秒。
- 统一所有外部链接按钮走异步打开逻辑，打开项目主页、反馈页、下载页都不会阻塞界面。
- 修复 macOS 顶部 toast 被 Overlay 标题栏遮挡的问题，toast 自动下移到标题栏安全区下方。

## [v0.2.14] - 2026-07-04

- 修复 macOS Overlay 标题栏遮挡内容的问题，为红黄绿窗口按钮预留完整安全区。
- 调整侧边栏、内容区、Provider/TOML/指令提示词/会话管理页高度计算，避免顶部内容被标题栏压住。
- 加高 macOS 顶部拖拽区域并保持深色渐变，窗口顶部继续和应用色系统一。

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
