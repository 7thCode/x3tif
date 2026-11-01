# project-template

Template repository for new projects: multi-platform build via
[actions-templates](https://github.com/7thCode/actions-templates), GitHub Release
publishing on tag push, and a GitHub Pages download page.

## Using this template

```bash
gh repo create <new-repo> --template 7thCode/project-template --public
cd <new-repo>
```

Then:

1. **Branch protection** — require PRs into `main`:
   ```bash
   scripts/setup-branch-protection.sh <owner>/<new-repo>
   ```
2. **GitHub Pages** — Settings → Pages → Source: Deploy from a branch →
   `main` / `/docs`.
3. **Fill in `docs/index.html`** — replace `PROJECT_NAME`, `PROJECT_DESCRIPTION`,
   `OWNER/REPO`, version and download links, `FEATURE_*`, `FOOTER_TEXT`.
4. **Adjust `.github/workflows/build.yml`** if the project isn't a plain
   `npm ci && npm run build && electron-builder` app — override the reusable
   workflow's `node_version` / `package_manager` / `build_command` /
   `artifact_globs` inputs (see
   [actions-templates README](https://github.com/7thCode/actions-templates)).
5. **Tag a release** — `git tag v0.1.0 && git push origin v0.1.0` builds on
   macOS/Windows/Linux and publishes installers to the matching GitHub Release.

## Workflow

Always work on a feature branch and merge into `main` via pull request — never
push directly to `main`.
