# publish-zed-extension

Composite GitHub Action that opens or updates a PR in
[`zed-industries/extensions`](https://github.com/zed-industries/extensions)
to bump this extension's submodule pointer and version.

## Why this exists (vs. `huacnlee/zed-extension-action`)

The upstream `huacnlee/zed-extension-action` creates a new branch with a
timestamped name on every run (`update-<ext>-<unix-time>`), which means
re-running the publish step opens a duplicate PR every time instead of updating
the in-flight one.

This action uses a deterministic branch name (`bump-<ext>-<version>`) and
checks for an existing open PR before deciding whether to create a new one or
force-push the existing branch. Net effect: **at most one open PR per version,
ever** — re-runs update the existing PR.

## Inputs

| Name              | Required | Description                                                                 |
| ----------------- | -------- | --------------------------------------------------------------------------- |
| `extension-name`  | yes      | Name as registered upstream (e.g., `laravel`).                              |
| `version`         | yes      | Version without `v` prefix. A tag `v<version>` must exist on the source repo. |
| `push-to`         | yes      | Fork of `zed-industries/extensions` to push to (e.g., `GeneaLabs/extensions`). Must already exist. |
| `upstream-repo`   | no       | Defaults to `zed-industries/extensions`.                                    |
| `committer-token` | yes      | PAT with `repo` + `workflow` scopes on the fork.                            |

## Behavior

1. Looks up any open PR from `<fork-owner>:bump-<ext>-<version>` to upstream.
2. Clones the fork, syncs its default branch with upstream (force-push), then
   builds the bump branch fresh from upstream's default branch.
3. Initializes the `extensions/<ext>` submodule, fetches tag `v<version>`,
   checks it out.
4. Updates `version = "<version>"` inside the `[<ext>]` section of
   `extensions.toml` (scoped — won't touch other entries).
5. Commits both changes if anything actually changed (idempotent).
6. Force-pushes the bump branch.
7. Creates a new PR if none existed, otherwise the force-push updates the
   existing PR automatically.

## Prerequisites

- Fork `zed-industries/extensions` to the `push-to` owner.
- Ensure the fork's `.gitmodules` for `extensions/<ext>` points at the source
  repo (true by default if the fork was made cleanly).
- Add a `ZED_PUBLISHING_TOKEN` repo secret with a PAT that has `repo` +
  `workflow` scopes on the fork's owner account.
- Push tag `v<version>` to the source repo *before* this action runs. In this
  repo's `release.yml`, the `update-version` job handles that and `publish`
  depends on it.

## Usage

```yaml
- uses: actions/checkout@v5
  with:
    ref: main

- uses: ./.github/actions/publish-zed-extension
  with:
    extension-name: laravel
    version: ${{ needs.update-version.outputs.new_version }}
    push-to: GeneaLabs/extensions
    committer-token: ${{ secrets.ZED_PUBLISHING_TOKEN }}
```
