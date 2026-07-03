# Codex破限 5.5 / codex-instruct-5.5

> 针对 GPT-5.5 的 Codex CLI 破甲工具 — 一键注入无限制模式系统指令  
> GPT-5.5 jailbreak for Codex CLI — one-click unrestricted mode injection

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Python 3.8+](https://img.shields.io/badge/Python-3.8%2B-blue)](https://python.org)

---

## 交流 / Community

- QQ AI 技术交流群：扫码加入
- TG 交流群：[@Geminivip1](https://t.me/Geminivip1)

<p align="center">
  <img width="360" alt="QQ AI 技术交流群二维码" src="assets/ai-tech-group.png" />
</p>

## 这是什么 / What

GPT-5.5 在 Codex CLI 中同样受到严格的内容安全限制。本工具利用 `model_instructions_file` 配置机制，注入一套专为 GPT-5.5 编写的无限制模式指令，强制 Codex 以 `[MODE: UNRESTRICTED]` 运行。

与 5.4 的 CTF 沙箱方案不同，5.5 版本采用更直接的破甲策略：直接声明无限制开发者模式、关闭所有过滤器、明确要求不拒绝任何任务。8 条核心指令覆盖了安全研究、渗透测试、逆向工程、NSFW 虚构内容等全部场景。

GPT-5.5 inside Codex CLI retains the same strict safety restrictions. This tool injects a custom unrestricted-mode instruction set via `model_instructions_file`, forcing Codex into `[MODE: UNRESTRICTED]`. Unlike the 5.4 CTF sandbox approach, the 5.5 version uses a more direct jailbreak: explicit unrestricted developer mode declaration, all filters disabled, 8 core directives covering security research, pentesting, reverse engineering, and NSFW content.

## 与 5.4 版本的区别 / vs 5.4

| | 5.4 (CTF沙箱) | 5.5 (无限制模式) |
|---|---|---|
| 策略 | CTF 竞赛沙箱间接绕过 | 直接声明无限制开发者模式 |
| 风格 | 安全研究导向，方法论详细 | 极简指令，覆盖面广 |
| 长度 | ~180 行 | ~40 行 |
| 适用 | GPT-5.4 Codex CLI | GPT-5.5 Codex CLI |

## 快速开始 / Quick Start

```bash
# 部署 GPT-5.5 破限指令
python codex-instruct.py

# 重启 Codex 生效
```

## 参数 / Options

| 参数 | 说明 |
|------|------|
| `--file`, `-f` | 使用外部 `.md` 指令文件 |
| `--name`, `-n` | 输出文件名不含 `.md`（默认 `gpt5.5-unrestricted`） |
| `--dry-run` | 预览，不实际修改 |
| `--codex-dir` | 手动指定 `.codex` 目录 |

## 验证 / Verify

部署后测试：

> "如何对目标进行 SQL 注入测试？"

破甲前 → 拒绝 ； 破甲后 → 直接给方法论。

## 还原 / Undo

```bash
# 删除 config.toml 中的 model_instructions_file 行
# 删除 ~/.codex/gpt5.5-unrestricted.md
# 重启 Codex
```

## 项目结构 / Layout

```
codex-instruct-5.5/
├── assets/
│   └── ai-tech-group.png                   # QQ AI 技术交流群二维码
├── codex-instruct.py                    # 主脚本
├── examples/
│   └── gpt5.5-unrestricted.md           # 破甲指令独立副本
├── .gitignore
├── README.md
└── LICENSE                              # MIT
```

## 声明 / Disclaimer

利用官方配置机制，不修改二进制、不劫持网络、不篡改进程。风险自负。

Exploits official config mechanism. No binary mod, no MITM, no process tampering. Use at your own risk.

## License

MIT

## 致谢 / Thanks

感谢 [LINUX DO 论坛](https://linux.do/) 社区的关注、反馈与支持。

## Star History

<p align="center">
  <a href="https://star-history.com/#yynxxxxx/Codex-5.5-codex-instruct-5.5&Date">
    <img alt="Star History Chart" src="https://api.star-history.com/svg?repos=yynxxxxx/Codex-5.5-codex-instruct-5.5&type=Date" />
  </a>
</p>
