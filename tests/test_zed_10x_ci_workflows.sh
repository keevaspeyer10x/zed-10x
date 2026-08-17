#!/usr/bin/env bash
# shellcheck disable=SC2016
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
CI="$ROOT/.github/workflows/ci-gate.yml"
RUST_CI="$ROOT/.github/workflows/zed-10x-ci.yml"
REVIEW="$ROOT/.github/workflows/minds-review-authorize.yml"
POLICY_HELPER="$ROOT/.github/scripts/minds-review-authorize.sh"
LEGACY="$ROOT/.github/workflows/minds-review.yml"

pass=0
fail=0

check() {
    local name=$1
    shift
    if "$@"; then
        printf 'PASS: %s\n' "$name"
        pass=$((pass + 1))
    else
        printf 'FAIL: %s\n' "$name"
        fail=$((fail + 1))
    fi
}

contains() {
    grep -Fq -- "$2" "$1"
}

not_contains() {
    local rc
    [[ -r "$1" && ! -L "$1" ]] || return 2
    if grep -Fq -- "$2" "$1"; then
        return 1
    else
        rc=$?
    fi
    [[ "$rc" -eq 1 ]] && return 0
    return "$rc"
}

check "standard CI caller exists" test -f "$CI"
check "standard CI caller handles reopened pull requests" \
    contains "$CI" "types: [opened, synchronize, reopened, ready_for_review]"
check "standard gate is pinned by immutable SHA" \
    contains "$CI" "agent-ci-gate.yml@8f20df85591ac1c14f6c9b1986cfdc9220bf29c1"
check "standard scan is pinned by the same immutable SHA" \
    contains "$CI" "agent-ci-gate-scan.yml@8f20df85591ac1c14f6c9b1986cfdc9220bf29c1"

check "focused Rust CI exists" test -f "$RUST_CI"
if [[ -f "$RUST_CI" ]]; then
    check "focused Rust CI targets main pull requests" \
        contains "$RUST_CI" "branches: [main]"
    check "focused Rust CI handles reopened pull requests" \
        contains "$RUST_CI" "types: [opened, synchronize, reopened, ready_for_review]"
    check "focused Rust CI has no workflow-level paths filter" \
        not_contains "$RUST_CI" "paths:"
    check "focused Rust CI has no workflow-level paths-ignore filter" \
        not_contains "$RUST_CI" "paths-ignore:"
    check "focused Rust CI grants read-only repository contents" \
        contains "$RUST_CI" "contents: read"
    check "focused Rust CI uses the bounded GitHub-hosted runner" \
        contains "$RUST_CI" "runs-on: ubuntu-24.04"
    check "focused Rust CI has a bounded timeout" \
        contains "$RUST_CI" "timeout-minutes: 120"
    check "checkout is pinned by immutable SHA" \
        contains "$RUST_CI" "actions/checkout@93cb6efe18208431cddfb8368fd83d5badbf9bfd"
    check "Linux dependencies use the repository setup script" \
        contains "$RUST_CI" "./script/linux"
    check "format command is exact" \
        contains "$RUST_CI" "cargo fmt --all -- --check"
    check "focused Zed check command is exact" \
        contains "$RUST_CI" "cargo check --locked -p zed --bin zed-10x"
    check "focused Zed test command is exact" \
        contains "$RUST_CI" "run: cargo test --locked -p zed --bin zed-10x"
    check "focused Zed tests are not filtered to an absent module" \
        not_contains "$RUST_CI" "reliability::hang_detection::liveness::tests"
    check "focused Rust CI has no cache action" \
        not_contains "$RUST_CI" "cache"
    check "focused Rust CI has no secret interpolation" \
        not_contains "$RUST_CI" 'secrets.'
    check "focused Rust CI has no custom runner" \
        not_contains "$RUST_CI" "namespace-"
fi

check "trusted-base review workflow exists" test -f "$REVIEW"
check "trusted-base review workflow handles reopened pull requests" \
    contains "$REVIEW" "types: [opened, synchronize, reopened, ready_for_review, labeled, unlabeled]"
check "trusted-base review delegates to the canonical router" \
    contains "$REVIEW" "uses: keevaspeyer10x/keeva-devtools/.github/workflows/minds-review-authorize-router.yml@b7f30483dc0cb4f35de20d9310cf639443dd5ef5"
check "trusted-base review maps only the review-host secret" \
    contains "$REVIEW" 'INTREPID_SSH_KEY: ${{ secrets.INTREPID_SSH_KEY }}'
check "trusted-base review workflow never checks out pull-request code" \
    not_contains "$REVIEW" "uses: actions/checkout"
check "standalone review policy is absent" test ! -e "$POLICY_HELPER"
check "legacy review workflow is absent" test ! -e "$LEGACY"

printf '\nResult: %s passed, %s failed\n' "$pass" "$fail"
((fail == 0))
