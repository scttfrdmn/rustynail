#!/usr/bin/env bash
#
# Release consistency gate.
#
# Every release artifact must agree on one version. Three separate incidents
# motivated this check:
#
#   1. v0.14.0 shipped a CHANGELOG entry and version bump but was never tagged
#      or released (#94) — the version existed only in files.
#   2. v0.5.0, v0.6.0 and v0.7.0 were tagged but had no GitHub release, and
#      v0.7.0 had no milestone at all. Nothing noticed for four months.
#   3. Docker builds failed at every tag from v0.9.0 onward because the builder
#      image MSRV was below what the lockfile required, so no tag ever produced
#      a usable image.
#
# All three were invisible because release steps were manual and unverified.
# This script is the verification. It runs in CI on every push and PR (version
# coherence only) and in full mode before a release.
#
# Usage:
#   scripts/check-release-consistency.sh            # local/CI: files must agree
#   scripts/check-release-consistency.sh --release  # also require tag/release/milestone
#
# Exit 0 = consistent, 1 = drift found.

set -uo pipefail

RELEASE_MODE=0
[[ "${1:-}" == "--release" ]] && RELEASE_MODE=1

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 1

FAILED=0
pass() { printf '[\xe2\x9c\x93] %s\n' "$1"; }
fail() { printf '[\xe2\x9c\x97] %s\n' "$1"; FAILED=1; }

# ── Source of truth ───────────────────────────────────────────────────────────

VERSION="$(grep -m1 '^version = ' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')"
if [[ -z "$VERSION" ]]; then
  fail "could not read version from Cargo.toml"
  exit 1
fi
echo "Cargo.toml version: $VERSION"
echo

# ── 1. Version coherence across files ─────────────────────────────────────────
# Each of these carried a stale version at some point in this repo's history.

check_contains() {
  local file="$1" pattern="$2" label="$3"
  if [[ ! -f "$file" ]]; then
    fail "$label: $file not found"
    return
  fi
  if grep -q "$pattern" "$file"; then
    pass "$label"
  else
    fail "$label: $file does not mention $VERSION"
  fi
}

check_contains "CHANGELOG.md" "^## \[$VERSION\]" "CHANGELOG has a [$VERSION] section"
check_contains "README.md" "$VERSION" "README references $VERSION"
check_contains "CLAUDE.md" "\*\*Version\*\*: $VERSION" "CLAUDE.md Version field is $VERSION"

# Cargo.lock must record the same version for this package, or a release build
# resolves something other than what Cargo.toml claims.
if grep -A1 '^name = "rustynail"$' Cargo.lock | grep -q "version = \"$VERSION\""; then
  pass "Cargo.lock rustynail entry is $VERSION"
else
  fail "Cargo.lock rustynail entry is not $VERSION (run: cargo update -p rustynail)"
fi

CHART="deploy/helm/rustynail/Chart.yaml"
if [[ -f "$CHART" ]]; then
  chart_ver="$(grep -m1 '^version:' "$CHART" | awk '{print $2}')"
  chart_app="$(grep -m1 '^appVersion:' "$CHART" | tr -d '"' | awk '{print $2}')"
  if [[ "$chart_ver" == "$VERSION" ]]; then
    pass "Helm chart version is $VERSION"
  else
    fail "Helm chart version is $chart_ver, expected $VERSION"
  fi
  if [[ "$chart_app" == "$VERSION" ]]; then
    pass "Helm chart appVersion is $VERSION"
  else
    fail "Helm chart appVersion is $chart_app, expected $VERSION"
  fi
fi

# ── 2. Changelog hygiene (Keep a Changelog 1.1.0) ─────────────────────────────

if grep -q '^## \[Unreleased\]' CHANGELOG.md; then
  pass "CHANGELOG has an [Unreleased] section"
else
  fail "CHANGELOG is missing the [Unreleased] section"
fi

# Only the six Keep a Changelog headers are permitted. CLAUDE.md forbids
# invented headers such as "### Planned" or "### Documentation".
bad_headers="$(grep '^### ' CHANGELOG.md \
  | grep -vE '^### (Added|Changed|Deprecated|Removed|Fixed|Security)$' \
  | sort -u)"
if [[ -z "$bad_headers" ]]; then
  pass "CHANGELOG uses only the six Keep a Changelog headers"
else
  fail "CHANGELOG has non-standard headers:"
  printf '      %s\n' "$bad_headers"
fi

# The comparison link for the current version must exist, or the changelog
# footer silently rots as versions accumulate.
if grep -q "^\[$VERSION\]:" CHANGELOG.md; then
  pass "CHANGELOG has a comparison link for $VERSION"
else
  fail "CHANGELOG has no [$VERSION]: comparison link at the bottom"
fi

# ── 3. MSRV / Dockerfile agreement ────────────────────────────────────────────
# Incident 3: Dockerfile builder was rust:1.75 while dependencies needed 1.94.
# CI used `stable` so it never noticed; only tag builds broke.

MSRV="$(grep -m1 '^rust-version = ' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')"
if [[ -z "$MSRV" ]]; then
  fail "Cargo.toml has no rust-version (MSRV) field"
else
  pass "Cargo.toml declares MSRV $MSRV"
  if [[ -f Dockerfile ]]; then
    docker_rust="$(grep -m1 -oE 'rust:[0-9]+\.[0-9]+(\.[0-9]+)?' Dockerfile | cut -d: -f2)"
    if [[ -z "$docker_rust" ]]; then
      fail "could not determine builder Rust version from Dockerfile"
    elif [[ "$docker_rust" == "$MSRV"* || "$MSRV" == "$docker_rust"* ]]; then
      pass "Dockerfile builder Rust $docker_rust matches MSRV $MSRV"
    else
      fail "Dockerfile builder Rust is $docker_rust but MSRV is $MSRV"
    fi
  fi
fi

# ── 4. agenkit pin agreement ──────────────────────────────────────────────────
# agenkit is a path dependency, so Cargo cannot constrain its version. The
# workflow `ref:` is the only pin, and the released image must be built against
# the same revision CI validated.

ci_ref="$(grep -A6 'repository: scttfrdmn/agenkit' .github/workflows/ci.yml | grep -m1 'ref:' | awk '{print $2}')"
docker_ref="$(grep -A6 'repository: scttfrdmn/agenkit' .github/workflows/docker.yml | grep -m1 'ref:' | awk '{print $2}')"

if [[ -z "$ci_ref" ]]; then
  fail "ci.yml does not pin the agenkit checkout to a ref"
elif [[ "$ci_ref" == "main" || "$ci_ref" == "master" ]]; then
  fail "ci.yml pins agenkit to '$ci_ref' — pin a release tag, not a branch"
else
  pass "ci.yml pins agenkit to $ci_ref"
fi

if [[ "$ci_ref" == "$docker_ref" ]]; then
  pass "docker.yml agenkit pin matches ci.yml ($docker_ref)"
else
  fail "agenkit pin drift: ci.yml=$ci_ref docker.yml=$docker_ref"
fi

# ── 5. Release-only checks ────────────────────────────────────────────────────
# Incidents 1 and 2. Requires `gh` and network access, so it is gated behind
# --release rather than running on every PR.

if [[ "$RELEASE_MODE" == "1" ]]; then
  echo
  echo "Release checks:"

  if ! command -v gh >/dev/null 2>&1; then
    fail "gh CLI not available; cannot verify tag/release/milestone"
  else
    # A tag must exist for this version and point at a commit on main.
    if git rev-parse "v$VERSION" >/dev/null 2>&1; then
      pass "tag v$VERSION exists"
      if git merge-base --is-ancestor "v$VERSION" origin/main 2>/dev/null; then
        pass "tag v$VERSION is an ancestor of origin/main"
      else
        fail "tag v$VERSION is not on origin/main"
      fi
    else
      fail "tag v$VERSION does not exist (incident #94: version bumped, never tagged)"
    fi

    # Every tag needs a GitHub release. v0.5.0/v0.6.0/v0.7.0 went four months
    # without one.
    if gh release view "v$VERSION" >/dev/null 2>&1; then
      pass "GitHub release v$VERSION exists"
    else
      fail "no GitHub release for v$VERSION"
    fi

    orphan_tags="$(comm -23 \
      <(git tag -l 'v*' | sort) \
      <(gh release list --limit 100 --json tagName -q '.[].tagName' | sort))"
    if [[ -z "$orphan_tags" ]]; then
      pass "every v* tag has a corresponding GitHub release"
    else
      fail "tags with no GitHub release:"
      printf '      %s\n' "$orphan_tags"
    fi

    # A milestone should exist and be closed for a shipped version.
    ms_state="$(gh api "repos/scttfrdmn/rustynail/milestones?state=all&per_page=100" \
      --jq ".[] | select(.title | startswith(\"v$VERSION\")) | .state" 2>/dev/null | head -1)"
    case "$ms_state" in
      closed) pass "milestone v$VERSION exists and is closed" ;;
      open)   fail "milestone v$VERSION is still open" ;;
      *)      fail "no milestone found for v$VERSION" ;;
    esac

    # A closed milestone with open issues means the release claims work it
    # did not ship.
    stragglers="$(gh api "repos/scttfrdmn/rustynail/milestones?state=all&per_page=100" \
      --jq '.[] | select(.state=="closed" and .open_issues>0) | "\(.title) (\(.open_issues) open)"' 2>/dev/null)"
    if [[ -z "$stragglers" ]]; then
      pass "no closed milestone has open issues"
    else
      fail "closed milestones with open issues:"
      printf '      %s\n' "$stragglers"
    fi
  fi
fi

echo
if [[ "$FAILED" == "0" ]]; then
  echo "All release consistency checks passed."
else
  echo "Release consistency check FAILED — see [x] lines above."
fi
exit "$FAILED"
