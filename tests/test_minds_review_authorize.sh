#!/usr/bin/env bash
# shellcheck disable=SC2016
# Consumer conformance for the canonical reusable minds-review router.
# Behavioural policy tests stay in keeva-devtools; this file proves that Zed
# exposes only the reviewed thin-caller interface.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CALLER="$DIR/.github/workflows/minds-review-authorize.yml"
HELPER="$DIR/.github/scripts/minds-review-authorize.sh"
LEGACY="$DIR/.github/workflows/minds-review.yml"
CI_WORKFLOW="$DIR/.github/workflows/ci-gate.yml"
EXPECTED_CALLER_BYTES=1948
EXPECTED_CALLER_SHA256="13d9b59cef7ff4ff0a98e3bdd9009820d007bf5dfbf284e7ed46c93f47cfc2da"

PASS=0
FAIL=0

check() {
    local name="$1"
    shift
    if "$@"; then
        printf 'PASS: %s\n' "$name"
        PASS=$((PASS + 1))
    else
        printf 'FAIL: %s\n' "$name"
        FAIL=$((FAIL + 1))
    fi
}

contains() {
    grep -Fq -- "$2" "$1"
}

not_contains() {
    ! grep -Fq -- "$2" "$1"
}

matches() {
    grep -Eq -- "$2" "$1"
}

not_matches() {
    ! grep -Eq -- "$2" "$1"
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

exact_caller_contract() {
    local candidate="$1"
    local byte_count
    local actual_sha

    [[ -f "$candidate" && ! -L "$candidate" && -r "$candidate" ]] || return 2
    byte_count=$(wc -c <"$candidate" | tr -d '[:space:]') || return 2
    actual_sha=$(sha256_file "$candidate") || return 2
    if [[ "$byte_count" == "$EXPECTED_CALLER_BYTES" ]] &&
        [[ "$actual_sha" == "$EXPECTED_CALLER_SHA256" ]]; then
        return 0
    fi
    return 1
}

not_exact_caller_contract() {
    local rc
    if exact_caller_contract "$1"; then
        rc=0
    else
        rc=$?
    fi
    case "$rc" in
        0) return 1 ;;
        1) return 0 ;;
        *) return "$rc" ;;
    esac
}

mutation_setup_error_is_rejected() {
    local rc
    if not_exact_caller_contract "$MUTATION_DIR/does-not-exist.yml"; then
        rc=0
    else
        rc=$?
    fi
    [[ "$rc" -eq 2 ]]
}

create_mutation() {
    local destination="$1"
    local program="$2"
    if ! awk "$program" "$CALLER" >"$destination"; then
        printf 'ERROR: failed to create mutation fixture %s\n' "$destination" >&2
        return 2
    fi
    if [[ ! -f "$destination" || -L "$destination" || ! -s "$destination" ]]; then
        printf 'ERROR: invalid mutation fixture %s\n' "$destination" >&2
        return 2
    fi
}

MUTATION_DIR=$(mktemp -d) || {
    printf 'ERROR: failed to create mutation directory\n' >&2
    exit 2
}
[[ -d "$MUTATION_DIR" && ! -L "$MUTATION_DIR" ]] || {
    printf 'ERROR: invalid mutation directory\n' >&2
    exit 2
}
trap 'rm -rf "$MUTATION_DIR"' EXIT

check "thin caller exists" test -f "$CALLER"
check "caller is byte-exact to the reviewed canonical consumer contract" \
    exact_caller_contract "$CALLER"
check "caller is a trusted-base event consumer" \
    contains "$CALLER" "pull_request_target:"
check "caller owns reopen, draft-ready, and label lifecycle" \
    contains "$CALLER" "types: [opened, synchronize, reopened, ready_for_review, labeled, unlabeled]"
check "caller carries the installed-router marker" \
    contains "$CALLER" "MINDS_REVIEW_ATOMIC_ROUTER_V1"
check "caller resolves the canonical reusable router" \
    contains "$CALLER" "uses: keevaspeyer10x/keeva-devtools/.github/workflows/minds-review-authorize-router.yml@b7f30483dc0cb4f35de20d9310cf639443dd5ef5"
check "caller serializes each exact pull-request head" \
    contains "$CALLER" 'group: minds-review-${{ github.event.pull_request.number }}-${{ github.event.pull_request.head.sha }}'
check "caller never cancels the admission projection" \
    contains "$CALLER" "cancel-in-progress: false"

check "caller grants contents read" \
    contains "$CALLER" "contents: read"
check "caller grants pull-request label writes" \
    contains "$CALLER" "pull-requests: write"
check "caller grants prerequisite check reads" \
    contains "$CALLER" "checks: read"
check "caller grants no issue writes" \
    not_contains "$CALLER" "issues: write"

check "Zed uses the canonical default typed inputs" \
    not_matches "$CALLER" '^[[:space:]]+with:[[:space:]]*$'
check "caller maps only the declared review-host secret" \
    contains "$CALLER" 'INTREPID_SSH_KEY: ${{ secrets.INTREPID_SSH_KEY }}'
check "caller never grants blanket secret access" \
    not_contains "$CALLER" "secrets: inherit"

check "caller never checks out pull-request code" \
    not_contains "$CALLER" "uses: actions/checkout"
check "caller contains no executable PR-head step" \
    not_matches "$CALLER" '^[[:space:]]+(run|shell):[[:space:]]*'
check "caller contains no local authorization implementation" \
    not_contains "$CALLER" "minds_authorize_decision"
check "standalone policy helper is absent" test ! -e "$HELPER"
check "legacy review workflow is absent" test ! -e "$LEGACY"

check "standard blocking CI uses the immutable gate" \
    contains "$CI_WORKFLOW" "agent-ci-gate.yml@8f20df85591ac1c14f6c9b1986cfdc9220bf29c1"
check "standard blocking CI uses the immutable scanner" \
    contains "$CI_WORKFLOW" "agent-ci-gate-scan.yml@8f20df85591ac1c14f6c9b1986cfdc9220bf29c1"

# The digest is the fail-closed contract. These mutation fixtures prove that a
# superficially compliant caller cannot add another trigger, permission, typed
# input, secret mapping, or executable PR-head surface while retaining the
# reviewed consumer identity.
check "fixture setup errors are not accepted as contract mismatches" \
    mutation_setup_error_is_rejected

create_mutation "$MUTATION_DIR/extra-trigger.yml" \
    '{ print; if ($0 ~ /^    types:/) print "  workflow_dispatch:" }'
check "exact contract rejects an additional trigger" \
    not_exact_caller_contract "$MUTATION_DIR/extra-trigger.yml"

create_mutation "$MUTATION_DIR/extra-permission.yml" \
    '{ print; if ($0 == "  checks: read") print "  issues: write" }'
check "exact contract rejects an additional permission" \
    not_exact_caller_contract "$MUTATION_DIR/extra-permission.yml"

create_mutation "$MUTATION_DIR/extra-input.yml" \
    '{ print; if ($0 ~ /minds-review-authorize-router.yml@b7f30483dc0cb4f35de20d9310cf639443dd5ef5$/) { print "    with:"; print "      admission: repo-runner" } }'
check "exact contract rejects a typed input override" \
    not_exact_caller_contract "$MUTATION_DIR/extra-input.yml"

create_mutation "$MUTATION_DIR/extra-secret.yml" \
    '{ print; if ($0 ~ /INTREPID_SSH_KEY:/) print "      EXTRA_SECRET: ${{ secrets.EXTRA_SECRET }}" }'
check "exact contract rejects an additional secret mapping" \
    not_exact_caller_contract "$MUTATION_DIR/extra-secret.yml"

create_mutation "$MUTATION_DIR/pr-head-execution.yml" \
    '{ print; if ($0 ~ /minds-review-authorize-router.yml@b7f30483dc0cb4f35de20d9310cf639443dd5ef5$/) print "    run: ./untrusted-pr-head" }'
check "exact contract rejects executable PR-head surface" \
    not_exact_caller_contract "$MUTATION_DIR/pr-head-execution.yml"

printf '\nResult: %s passed, %s failed\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]]
