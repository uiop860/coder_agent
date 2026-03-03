# Git Commit Message Guide

## Critical Rule

**OUTPUT EXACTLY ONE COMMIT MESSAGE. NOTHING ELSE.**

No explanations, no questions, no additional text before or after. Just the commit message.

## Format

```
<emoji> <type>(<scope>): <description>

[optional body with bullet points]

[optional footer(s)]
```

- **Max 100 characters per line**
- **All text must be in English**
- **Imperative mood** (e.g., "add" not "added")
- **No period at end of description**

## Types

| Type     | Emoji | Description                                  |
| -------- | ----- | -------------------------------------------- |
| feat     | ✨    | New feature                                  |
| fix      | 🐛    | Bug fix                                      |
| docs     | 📝    | Documentation changes                        |
| style    | 💄    | Code style (formatting, whitespace)          |
| refactor | ♻️    | Code restructuring (no feature/fix)          |
| perf     | ⚡️    | Performance improvement                      |
| test     | ✅    | Tests                                        |
| build    | 🏗️    | Build system or dependencies                 |
| ci       | 👷    | CI configuration                             |
| chore    | 🔧    | Miscellaneous (scripts, config)              |
| revert   | ⏪️    | Revert previous commit                       |

## Scope

- Optional but recommended when change affects specific component
- Examples: `auth`, `api`, `database`, `deps`
- Omit if change affects entire project

## Body

- Use bullet points with `-`
- Describe WHAT changed (based on diff)
- Explain WHY only if evident from context
- Max 100 characters per line

## Footer

Common footers:
- `Fixes #123` - Closes issue
- `BREAKING CHANGE: description` - Breaking changes
- `Co-authored-by: Name <email>` - Multiple authors

## Dependency Updates

When updating dependencies:
- **Only list DIRECT dependencies** from manifest files (package.json, pyproject.toml, etc.)
- **Ignore lockfile-only changes** (pnpm-lock.yaml, Cargo.lock, etc.)

Example:
```
🔧 chore(deps): update @tanstack packages

- @tanstack/react-router: 1.133.15 → 1.133.21
- @tanstack/router-cli: 1.133.15 → 1.133.20
```

## Examples

### Simple Change
```
♻️ refactor(server): use environment variable for port configuration

- rename port variable to uppercase (PORT)
- use process.env.PORT with fallback to default (7799)
```

### Bug Fix
```
🐛 fix(auth): prevent token expiration during active sessions

- extend token TTL on user activity
- add background refresh job

Fixes #234
```

### Multiple Unrelated Changes

Use this format ONLY for completely unrelated changes (e.g., fix auth bug + update docs + add feature):

```
🐛 fix(auth): resolve login redirect loop

- clear stale session tokens before redirect

💄 style(navbar): adjust background opacity

- change from /10 to /15

📝 docs: update API endpoint documentation
```

**If changes are related, use ONE commit with detailed body instead.**

## Remember

**OUTPUT ONLY THE COMMIT MESSAGE. NO OTHER TEXT.**

- DO NOT wrap the output in ``` code blocks
- DO NOT add any formatting delimiters or special characters
- The commit message should be raw text, ready to use directly
