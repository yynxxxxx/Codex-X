pub(crate) const INSTRUCTION_FILENAME: &str = "gpt5.5-unrestricted.md";
pub(crate) const INSTRUCTION_CONTENT: &str =
    include_str!("../../../../examples/gpt5.5-unrestricted.md");

pub(crate) const INSTRUCTION_54_FILENAME: &str = "gpt5.4-unrestricted.md";
pub(crate) const INSTRUCTION_54_CONTENT: &str =
    include_str!("../../../../examples/gpt5.4-unrestricted.md");

pub(crate) const INSTRUCTION_JELI_FILENAME: &str = "gpt5.5-jeli.md";
pub(crate) const INSTRUCTION_JELI_CONTENT: &str =
    include_str!("../../../../examples/gpt5.5-jeli.md");

pub(crate) const INSTRUCTION_56_SOL_FILENAME: &str = "gpt-5.6-sol-unrestricted.md";
pub(crate) const INSTRUCTION_56_SOL_CONTENT: &str =
    include_str!("../../../../examples/gpt-5.6-sol-unrestricted.md");

pub(crate) const INSTRUCTION_SEAGULL_FILENAME: &str = "海鸥3.0破甲.md";
pub(crate) const INSTRUCTION_SEAGULL_CONTENT: &str =
    include_str!("../../../../examples/海鸥3.0破甲.md");

pub(crate) const AGENTS_FILENAME: &str = "AGENTS.md";
pub(crate) const AGENTS_MANAGED_BEGIN: &str = "<!-- CODEX-X:INSTRUCTIONS:BEGIN -->";
pub(crate) const AGENTS_MANAGED_END: &str = "<!-- CODEX-X:INSTRUCTIONS:END -->";
pub(crate) const AGENTS_TEMPLATE_PREFIX: &str = "<!-- CODEX-X:TEMPLATE:";
pub(crate) const JSDELIVR_EXAMPLES_API: &str =
    "https://data.jsdelivr.com/v1/packages/gh/yynxxxxx/Codex-X@main?structure=flat";
pub(crate) const JSDELIVR_EXAMPLES_BASE: &str =
    "https://cdn.jsdelivr.net/gh/yynxxxxx/Codex-X@main/examples/";
pub(crate) const GITHUB_EXAMPLES_API: &str =
    "https://api.github.com/repos/yynxxxxx/Codex-X/contents/examples?ref=main";
pub(crate) const GITHUB_EXAMPLES_BASE: &str =
    "https://raw.githubusercontent.com/yynxxxxx/Codex-X/main/examples/";

pub(crate) const MAX_SKILL_ZIP_BYTES: u64 = 20 * 1024 * 1024;
