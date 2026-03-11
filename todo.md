# tasks

- Fuzzy file search — search_file is glob-only. A fuzzy matcher (like nucleo) would make it much more useful for large codebases.
- improve replace_lines tool
  - replacing multiple lines from multiple tool calls fails since the line placements change


# other tool apply_patch implementations

- opencode: https://github.com/anomalyco/opencode/blob/22a4c5a77e466c6a81f7c461a58a7e63cd91be45/packages/opencode/src/tool/edit.ts
 - https://raw.githubusercontent.com/anomalyco/opencode/22a4c5a77e466c6a81f7c461a58a7e63cd91be45/packages/opencode/src/tool/edit.ts
- codex: https://github.com/openai/codex/tree/main/codex-rs/apply-patch
