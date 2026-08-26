#!/usr/bin/env bash
# shellcheck disable=SC2016
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
CI="$ROOT/.github/workflows/ci-gate.yml"
RUST_CI="$ROOT/.github/workflows/zed-10x-ci.yml"
RELEASE="$ROOT/.github/workflows/zed-10x-release.yml"
UPSTREAM_EXIT="$ROOT/.github/workflows/zed-10x-upstream-exit.yml"
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

not_matches() {
    local rc
    [[ -r "$1" && ! -L "$1" ]] || return 2
    if grep -Eq -- "$2" "$1"; then
        return 1
    else
        rc=$?
    fi
    [[ "$rc" -eq 1 ]] && return 0
    return "$rc"
}

check "unresolvable private reusable CI caller is absent" test ! -e "$CI"

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
    check "focused CI runs its workflow contract" \
        contains "$RUST_CI" "run: tests/test_zed_10x_ci_workflows.sh"
    check "focused CI pins the repository Node.js version" \
        contains "$RUST_CI" "node-version: \"24\""
    check "focused CI runs Zed 10x Node regression tests" \
        contains "$RUST_CI" "node --test script/tests/*.test.mjs"
    check "Linux dependencies use the repository setup script" \
        contains "$RUST_CI" "./script/linux"
    check "format command is exact" \
        contains "$RUST_CI" "cargo fmt --all -- --check"
    check "focused CI runs custom agent alias resolution tests" \
        contains "$RUST_CI" "cargo test --locked -p project custom_agent_aliases_override_colliding_registry_ids_when_unambiguous --lib"
    check "focused CI runs custom agent alias ownership tests" \
        contains "$RUST_CI" "cargo test --locked -p project explicit_custom_alias_supersedes_colliding_registry_agent --lib"
    check "focused CI runs registry host display tests" \
        contains "$RUST_CI" "cargo test --locked -p project registry_display_name --lib"
    check "focused CI runs remote agent ordering tests" \
        contains "$RUST_CI" "cargo test --locked -p project remote_agent_ --lib"
    check "focused CI protects execution-host agent settings" \
        contains "$RUST_CI" "cargo test --locked -p project remote_execution_host_ --lib"
    check "focused CI runs authoritative remote agent metadata tests" \
        contains "$RUST_CI" "cargo test --locked -p remote_server test_remote_external_agent_metadata_comes_from_execution_host --lib"
    check "focused CI runs remote reconnect inventory tests" \
        contains "$RUST_CI" "cargo test --locked -p remote_server test_remote_external_agent_server_reconnects_to_latest_inventory --lib"
    check "focused CI rejects client agents after remote reconnect" \
        contains "$RUST_CI" "cargo test --locked -p remote_server test_remote_client_settings_never_replace_execution_host_agents --lib"
    check "focused CI runs custom agent cache compatibility tests" \
        contains "$RUST_CI" "cargo test --locked -p agent_ui equivalent_cache_key_prefers_canonical_and_recognizes_legacy_aliases --lib"
    check "focused CI runs restored thread canonicalization tests" \
        contains "$RUST_CI" "cargo test --locked -p agent_ui test_store_update_agent_id_preserves_thread_identity --lib"
    check "focused CI runs restored thread load-error integration tests" \
        contains "$RUST_CI" "cargo test --locked -p agent_ui test_serialize_preserves_session_id_in_load_error --lib"
    check "focused CI runs custom agent rename tests" \
        contains "$RUST_CI" "cargo test --locked -p settings_ui rename_preserves_original_name_as_alias --lib"
    check "focused CI runs custom agent rename-back tests" \
        contains "$RUST_CI" "cargo test --locked -p settings_ui rename_back_drops_the_new_canonical_name_from_aliases --lib"
    check "focused CI tests the fork updater" \
        contains "$RUST_CI" "cargo test --locked -p auto_update zed_10x"
    check "focused CI tests release-channel behavior" \
        contains "$RUST_CI" "cargo test --locked -p release_channel"
    check "focused Zed check command is exact" \
        contains "$RUST_CI" "cargo check --locked -p zed --bin zed-10x"
    check "focused Zed test command is exact" \
        contains "$RUST_CI" "run: cargo test --locked -p zed --bin zed-10x -- --test-threads=1"
    check "focused Zed tests are not filtered to an absent module" \
        not_contains "$RUST_CI" "reliability::hang_detection::liveness::tests"
    check "focused Rust CI has no cache action" \
        not_matches "$RUST_CI" '^[[:space:]]*-?[[:space:]]*uses:[[:space:]]*[^#]*[Cc]ache@'
    check "focused Rust CI has no secret interpolation" \
        not_contains "$RUST_CI" 'secrets.'
    check "focused Rust CI has no custom runner" \
        not_contains "$RUST_CI" "namespace-"
fi

check "fork-owned macOS release workflow exists" test -f "$RELEASE"
if [[ -f "$RELEASE" ]]; then
    check "release workflow is manual only" \
        contains "$RELEASE" "workflow_dispatch:"
    check "release workflow has no repository write permission" \
        contains "$RELEASE" "contents: read"
    check "release workflow uses a GitHub-hosted macOS runner" \
        contains "$RELEASE" "runs-on: macos-15"
    check "release workflow runs only for protected fork main" \
        contains "$RELEASE" "github.repository == 'keevaspeyer10x/zed-10x' && github.ref == 'refs/heads/main'"
    check "release credentials come from the release environment" \
        contains "$RELEASE" "environment: zed-10x-release"
    check "release checkout is pinned by immutable SHA" \
        contains "$RELEASE" "actions/checkout@93cb6efe18208431cddfb8368fd83d5badbf9bfd"
    check "release build is credential-free" \
        contains "$RELEASE" "./script/bundle-mac --zed-10x-prepare-release aarch64-apple-darwin"
    check "release signing is a separate narrow step" \
        contains "$RELEASE" "./script/sign-zed-10x-macos-release aarch64-apple-darwin"
    check "release signing uses a fresh runner after unsigned build" \
        contains "$RELEASE" "needs: unsigned-macos-aarch64"
    check "unsigned inputs cross jobs through a pinned artifact action" \
        contains "$RELEASE" "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c"
    check "release workflow retains the verification receipt" \
        contains "$RELEASE" "target/aarch64-apple-darwin/release/zed-10x-release-receipt.json"
    check "release publication waits for the protected signer" \
        contains "$RELEASE" "needs: [remote-server-linux, signed-macos-aarch64]"
    check "release builds matching Linux remote servers" \
        contains "$RELEASE" "zed-remote-server-linux-x86_64.gz"
    check "release builds the matching Linux ARM remote server" \
        contains "$RELEASE" "zed-remote-server-linux-aarch64.gz"
    check "release publication is restricted to protected fork main" \
        contains "$RELEASE" "github.repository == 'keevaspeyer10x/zed-10x' && github.ref == 'refs/heads/main'"
    check "release publication has a narrow write-capable job" \
        contains "$RELEASE" "contents: write"
    check "release publication rebinds the signed receipt" \
        contains "$RELEASE" "verify-zed-10x-release-publication.mjs"
    check "release publication starts as a draft" \
        contains "$RELEASE" "gh release create"
    check "release publication resumes only an exact draft" \
        contains "$RELEASE" "--expected-state draft"
    check "release publication safely replaces draft assets" \
        contains "$RELEASE" "--clobber"
    check "release publication verifies the complete draft before publishing" \
        contains "$RELEASE" "--expected-state draft-uploaded"
    check "release publication requires immutable metadata" \
        contains "$RELEASE" "--expected-state immutable"
    check "release publication requires GitHub asset digests" \
        contains "$RELEASE" "--github-release-json"
fi

check "read-only upstream exit workflow exists" test -f "$UPSTREAM_EXIT"
if [[ -f "$UPSTREAM_EXIT" ]]; then
    check "upstream exit monitor grants read-only repository contents" \
        contains "$UPSTREAM_EXIT" "contents: read"
    check "upstream exit monitor is weekly and manually observable" \
        contains "$UPSTREAM_EXIT" "workflow_dispatch:"
    check "upstream exit monitor runs only for protected fork main" \
        contains "$UPSTREAM_EXIT" "github.repository == 'keevaspeyer10x/zed-10x' && github.ref == 'refs/heads/main'"
    check "upstream exit monitor runs its regression tests" \
        contains "$UPSTREAM_EXIT" "zed-10x-upstream-exit-monitor.test.mjs"
    check "upstream exit monitor has no repository write authority" \
        not_contains "$UPSTREAM_EXIT" "contents: write"
    check "upstream exit monitor has no issue mutation command" \
        not_contains "$UPSTREAM_EXIT" "gh issue"
fi

check "unresolvable private reusable review caller is absent" test ! -e "$REVIEW"
check "standalone review policy is absent" test ! -e "$POLICY_HELPER"
check "legacy review workflow is absent" test ! -e "$LEGACY"

printf '\nResult: %s passed, %s failed\n' "$pass" "$fail"
((fail == 0))
