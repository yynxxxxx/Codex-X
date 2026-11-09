# 在 Windows 查看 WSL 会话

从 v0.3.31 起，可以在 Windows 版 Codex-X 中加载 WSL 的 Codex 会话，查看会话名称、首次提示词，并导出 Markdown 或 ZIP。

## 使用方法

1. 确认对应 WSL 发行版可以正常启动，并已安装 Python 3。Ubuntu / Debian 可在该发行版中运行 `sudo apt install python3`。
2. 在 Codex-X 的 `CODEX_HOME` 中选择该发行版的配置目录，例如 `\\wsl.localhost\Ubuntu\home\your-user\.codex`。`\\wsl$\Ubuntu\…` 也受支持。
3. 打开「会话管理」并点击「检查会话」。页面显示「WSL 会话 · 只读浏览」后，可以搜索、选择并导出会话。

会话列表优先显示已设置的会话名称；悬停在名称上可查看首次提示词。未设置名称时，继续使用现有标题作为回退。

## 当前范围

- WSL 会话仅支持浏览和导出。跨系统同步、自动同步和永久删除会被禁用，需要在对应 Linux 环境中操作。
- Windows 无法直接使用 WSL 会话库的数据库锁。Codex-X 会在所选发行版中读取一致性快照，并在读取后清理临时快照，不会把快照写回正在使用的数据库。
- 自定义 `sqlite_home` 需位于所选 `CODEX_HOME` 内；该限制只适用于 WSL 浏览功能。
- 未安装 Python、发行版启动失败、数据库损坏或读取超时时，会显示原因；不会把读取失败当成空会话列表或同步成功。

此功能不负责将 Windows Codex 客户端连接到 WSL，也不更改 Windows 或 WSL 的登录信息。
