#!/usr/bin/env bash
set -euo pipefail

# Required env (set by action.yml):
#   GH_TOKEN, EXT_NAME, VERSION, PUSH_TO, UPSTREAM, SOURCE_REPO

FORK_OWNER="${PUSH_TO%%/*}"
BRANCH="bump-${EXT_NAME}-${VERSION}"
SUBMODULE_PATH="extensions/${EXT_NAME}"

echo "::notice::Publishing ${EXT_NAME} v${VERSION} via ${PUSH_TO} → ${UPSTREAM} (branch: ${BRANCH})"

# Commit as the PAT owner so signed-CLA checks (e.g., Zed's cla-bot) resolve
# to a real human rather than github-actions[bot], which can't sign a CLA.
GIT_USER_LOGIN=$(gh api user --jq .login)
GIT_USER_ID=$(gh api user --jq .id)
GIT_USER_NAME=$(gh api user --jq '.name // .login')
git config --global user.name "${GIT_USER_NAME}"
git config --global user.email "${GIT_USER_ID}+${GIT_USER_LOGIN}@users.noreply.github.com"
echo "::notice::Committing as ${GIT_USER_NAME} <${GIT_USER_ID}+${GIT_USER_LOGIN}@users.noreply.github.com>"

EXISTING_PR=$(gh api "repos/${UPSTREAM}/pulls?state=open&head=${FORK_OWNER}:${BRANCH}" --jq '.[0].number // empty')
if [[ -n "${EXISTING_PR}" ]]; then
  echo "::notice::Found existing open PR #${EXISTING_PR} — branch will be force-updated"
else
  echo "::notice::No existing PR for ${BRANCH} — a new PR will be created"
fi

DEFAULT_BRANCH=$(gh repo view "${UPSTREAM}" --json defaultBranchRef --jq .defaultBranchRef.name)
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "${WORK_DIR}"' EXIT

git clone "https://x-access-token:${GH_TOKEN}@github.com/${PUSH_TO}.git" "${WORK_DIR}/fork"
cd "${WORK_DIR}/fork"

if [[ -n "${SIGNING_KEY:-}" ]]; then
  SSH_KEY_FILE="${WORK_DIR}/signing-key"
  printf '%s\n' "${SIGNING_KEY}" > "${SSH_KEY_FILE}"
  chmod 600 "${SSH_KEY_FILE}"
  git config --global gpg.format ssh
  git config --global user.signingkey "${SSH_KEY_FILE}"
  git config --global commit.gpgsign true
  echo "::notice::SSH commit signing enabled"
fi

git remote add upstream "https://github.com/${UPSTREAM}.git"
git fetch upstream "${DEFAULT_BRANCH}"

# Sync the fork's default branch with upstream so the PR base is current.
git checkout "${DEFAULT_BRANCH}"
git reset --hard "upstream/${DEFAULT_BRANCH}"
git push origin "${DEFAULT_BRANCH}"

# Always rebuild the bump branch from a fresh upstream base — cleaner than
# trying to rebase whatever was on the existing PR branch.
git checkout -B "${BRANCH}" "upstream/${DEFAULT_BRANCH}"

# Upstream's .gitmodules can record a stale submodule URL (e.g. after the
# source repo moved to a new org). Force it to the current source repo so the
# tag is fetched from the right place AND the bump PR carries the correction
# upstream. Self-healing for any future transfer/rename.
git config -f .gitmodules "submodule.${SUBMODULE_PATH}.url" "https://github.com/${SOURCE_REPO}.git"
git submodule sync "${SUBMODULE_PATH}"

git submodule update --init "${SUBMODULE_PATH}"
SOURCE_TAG=""
(
  cd "${SUBMODULE_PATH}"
  for candidate in "${VERSION}" "v${VERSION}"; do
    if git ls-remote --tags origin "refs/tags/${candidate}" | grep -q .; then
      echo "${candidate}" > /tmp/source-tag
      break
    fi
  done
  if [[ ! -s /tmp/source-tag ]]; then
    echo "::error::No tag matching '${VERSION}' or 'v${VERSION}' found on $(git remote get-url origin)"
    exit 1
  fi
  TAG="$(cat /tmp/source-tag)"
  echo "::notice::Using source tag '${TAG}'"
  git fetch --tags --depth 1 origin "refs/tags/${TAG}"
  git checkout "${TAG}"
)
SOURCE_TAG="$(cat /tmp/source-tag)"
rm -f /tmp/source-tag
git add "${SUBMODULE_PATH}"
git add .gitmodules

# Update the version line within the [<ext_name>] section only.
awk -v ext="${EXT_NAME}" -v ver="${VERSION}" '
  /^\[/ { in_section = ($0 == "[" ext "]") }
  in_section && /^version[[:space:]]*=/ {
    print "version = \"" ver "\""
    updated = 1
    next
  }
  { print }
  END { if (!updated) exit 1 }
' extensions.toml > extensions.toml.new
mv extensions.toml.new extensions.toml
git add extensions.toml

if git diff --cached --quiet; then
  echo "::notice::No changes to commit — ${EXT_NAME} already at v${VERSION} with matching submodule SHA"
  exit 0
fi

git commit -m "Bump ${EXT_NAME} to ${VERSION}

Release notes:
https://github.com/${SOURCE_REPO}/releases/tag/${SOURCE_TAG}"

git push origin "${BRANCH}" --force

if [[ -z "${EXISTING_PR}" ]]; then
  gh pr create \
    --repo "${UPSTREAM}" \
    --head "${FORK_OWNER}:${BRANCH}" \
    --base "${DEFAULT_BRANCH}" \
    --title "Bump ${EXT_NAME} to ${VERSION}" \
    --body "Bumps \`${EXT_NAME}\` to v${VERSION}.

Release notes: https://github.com/${SOURCE_REPO}/releases/tag/${SOURCE_TAG}"
  echo "::notice::Created new PR for ${EXT_NAME} v${VERSION}"
else
  echo "::notice::Force-pushed ${BRANCH} — PR #${EXISTING_PR} updated"
fi
