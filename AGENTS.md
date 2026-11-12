## Agent skills

### Issue tracker

Issues are tracked in Linear under the `kiku` project (`Shouryan Nikam` team). See `docs/agents/issue-tracker.md`.

### Domain docs

Single-context repo (`GLOSSARY.md` + `docs/adr/` at the repo root). See `docs/agents/domain.md`.

## Git, PRs & CI Conventions

- **Commits**: Short, capitalized, present affirmative voice (e.g., `Add core dictation session`, `Update CI actions to Node 24`).
- **PRs**: Capitalize the PR title. Keep the summary simple, clear, and close to ASD-STE100 (short sentences, plain words). Do not include `Evidence` or `Merge Danger` sections.
- **GitHub Actions**: Use Node 24-compatible action versions (`actions/checkout@v7`, `actions/upload-artifact@v7`, `actions/download-artifact@v8`).
