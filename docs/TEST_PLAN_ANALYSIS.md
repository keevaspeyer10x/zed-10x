# Test Plan Analysis — Root Cause Report

> **Date:** 2026-08-24
> **Test run:** 18 of 18 planned rows passed; 0 unresolved failures or skips
> **Analysis method:** DACI-RP over seven test-harness defects and four assembled-product defects discovered and fixed during deterministic and live execution

## Executive Summary

The full lifecycle did what the earlier release testing had not: it exercised every changed assembled-product surface through the installed app. It found seven harness defects and four product defects after the first component-level green. The product's last TCP correction now classifies real disconnect I/O in DAP header and body reads and replays initialize exactly once only for SSH/TCP, within the existing timeout; stdio and non-SSH behavior is unchanged. The final harness corrections made the fake adapter protocol-valid, made resize observation semantic rather than sample-position-dependent, authorized the fixture directory environment before project open, and prevented the cleanup observer from reporting its own ancestors. All findings were corrected and re-run. The installed app at product commit `658ad7c7` then completed terminal resize/input/Ctrl-C, task cancellation, Vim filtering, MCP initialization, stdio/TCP DAP request sequences, and real Mac-local and Intrepid ACP project reads without exposing private environment values or leaving owned processes. Commit `668b2ab4` changes only the tracked cleanup census; the exact merged commit must still be rebuilt and canaried after protected merge.

## Defect Discovery Value

- **Bugs found:** 11 product or harness defects, plus one earlier process defect
- **Product defects:** 4, all fixed and re-run through the installed app
- **Test harness defects:** 7, all fixed before accepting the UAT
- **Infra/tooling defects:** 0
- **Process defects:** 0 unresolved; the full lifecycle was executed rather than inferred from generated rows
- **Fixed during run:** SSH remote-command quoting; multi-tool project-evidence aggregation; resize-sensitive terminal fixture; captured shell-hook sanitization; remote Vim cwd; tagged SSH/TCP initialize replay; valid DAP response fixture; semantic resize oracle; pre-open direnv authorization; reuse-safe cleanup ancestry
- **Confidence-only rows:** 0
- **Weak rows:** 0
- **Low-yield assessment:** `high_confidence`
- **New information learned:**
  - the bundled and uploaded `658ad7c7` remote server advertises and executes both secure environment capabilities;
  - the real SSH PTY consumes a no-echo frame before shell input and preserves cwd, directory/terminal/task environment, stdin, resize, Ctrl-C, and cancellation semantics;
  - the installed Vim filter runs in the real project cwd and transforms selected text;
  - the installed MCP context server completes initialize, initialized notification, and tools/list;
  - the installed stdio and TCP debug adapters complete their real request sequences, and TCP classifies a delayed true reset, opens a second connection, and replays initialize exactly once;
  - real ACP agents may establish cwd and the sentinel digest in separate completed tool calls;
  - Codex Mac primary and Intrepid GLM can each read a withheld project sentinel and terminate cleanly with zero approved permissions.
- **Residual risk:**
  - public notarization is intentionally out of scope for this private installation;
  - post-merge acceptance must rebuild/install the exact merged commit and repeat the remote open plus focused canaries.

Evidence is sealed in:

- `docs/test-results/summary.json`
- `docs/test-results/test-validity.json`
- `docs/test-results/installed-uat.json`
- `docs/test-results/remote-environment-live-canary.json`
- `docs/test-results/acp-route-codex-mac-primary.json`
- `docs/test-results/acp-route-intrepid-glm.json`

## Root Cause Clusters

### Cluster 1: Remote command fragments were mistaken for an SSH argv contract

- **Affected row:** `ENV-DAP-TCP-013`
- **Category:** test bug
- **Severity:** high test-validity impact; no product defect
- **Symptom:** the live canary's remote port allocator invoked `ssh host /usr/bin/python3 -c <source>` as five local arguments. SSH concatenated those fragments into a remote shell command without preserving the Python source as one quoted argument, so the remote interpreter exited before the TCP journey.
- **5-Why chain:**
  1. The TCP live canary failed while noninteractive and PTY paths passed.
  2. Remote port allocation exited before the product protocol started.
  3. The Python `-c` source crossed SSH as unquoted shell fragments.
  4. The harness modeled SSH as direct `execve` argument transport instead of a remote-shell command boundary.
  5. Unit tests checked framing and content privacy but did not assert the harness's own production SSH argv shape.
- **Fix:** create one shell-quoted `/usr/bin/python3 -c …` command string and pass it as the sole post-host SSH argument.
- **Verification:** the new `remote port allocation sends one shell-quoted command to SSH` regression is RED on the former shape; 25/25 changed Node contracts pass; the live TCP receipt validates forwarded request/response hashes and cleanup.

### Cluster 2: The project oracle was coupled to one tool-call shape

- **Affected row:** `ACP-JOURNEY-002`
- **Category:** test bug
- **Severity:** high test-validity impact; no product defect
- **Symptom:** Intrepid GLM completed tool work but was rejected because cwd and sentinel digest did not appear in the same completed tool call.
- **5-Why chain:**
  1. A real agent started project tools but the canary reported `project_evidence_mismatch`.
  2. The canary evaluated each completed call independently.
  3. Agents legitimately use separate calls to inspect cwd and hash/read the sentinel.
  4. The test encoded one fixture's transcript shape as the user-visible contract.
  5. The prior fixture had only a single combined evidence call, so the overconstraint stayed invisible.
- **Fix:** evaluate the union of content-free evidence from completed tool calls while preserving all hard oracles: withheld digest, exact cwd, at least one completed tool call, terminal marker, zero approved permissions, and process-group cleanup.
- **Verification:** the new `split-evidence` fixture is RED on the former implementation; marker-only, prompt-echo, wrong-cwd, permission, auth, capacity, and timeout negatives remain green; the live Intrepid GLM run now passes.

### Cluster 3: The terminal fixture treated one signal wait as the process lifecycle

- **Affected row:** `ENV-TERMINAL-008`
- **Category:** test bug
- **Severity:** high test-validity impact; no product defect
- **Symptom:** resizing the installed terminal delivered SIGWINCH and returned the fixture from `signal.pause()`, so the process could exit before the later Ctrl-C oracle.
- **Generator:** the fixture modeled “wait for any signal” as “remain alive until cancellation.”
- **Fix:** keep the fixture in a signal wait loop and let the explicit interrupt handler terminate it.
- **Verification:** the installed terminal receipt records both a changed column count and `interrupted: true`; the exact fixture process identity is absent after close.

### Cluster 4: Captured shell state replayed a hook without its definition

- **Affected rows:** `ENV-TERMINAL-008`, `ENV-TASK-009`
- **Category:** product bug
- **Severity:** high user-visible startup defect
- **Symptom:** a fresh installed remote terminal immediately printed `__aisw_prompt_check: command not found`.
- **Generator:** directory-environment capture retained `PROMPT_COMMAND`, but shell functions referenced by that hook are not ordinary exported variables and were therefore absent in the new shell.
- **Fix:** remove captured `PROMPT_COMMAND` before applying explicit terminal or task environment settings; an explicit user setting remains authoritative because it is merged afterward.
- **Verification:** the focused unit test is green, the installed terminal opens without the warning, and task/terminal environment receipts remain exact.

### Cluster 5: Remote Vim shell execution omitted project cwd

- **Affected row:** `ENV-SHELL-010`
- **Category:** product bug
- **Severity:** high journey defect
- **Symptom:** the external filter received the correct environment but executed from `/home/keeva`, so project-relative commands were wrong.
- **Generator:** `exec_in_shell` migrated to secure environment transport but left the builder working directory unset.
- **Fix:** pass the first opened project directory into the secure remote command builder.
- **Verification:** the installed Vim filter ran from `/home/keeva/uat/zed-remote-environment-production`, transformed the selected text, and left no fixture process.

### Cluster 6: SSH port-forward reachability preceded adapter readiness

- **Affected row:** `ENV-DAP-TCP-013`
- **Category:** product bug
- **Severity:** high debug-session reliability defect
- **Symptom:** Zed connected to the local SSH forward before the remote adapter bound the destination port; the forwarded connection reset, but Zed accepted it as the session transport and hung while a later adapter listener remained unused.
- **Generator:** a successful local TCP connect was treated as proof that the remote adapter endpoint was stable.
- **Fix:** header and body read failures in the disconnect I/O class receive the existing connection-reset tag. The client replays `initialize` once only for SSH/TCP and uses the already documented SSH/TCP timeout as its overall deadline. Stdio and local TCP keep the ordinary request path.
- **Verification:** deterministic clean-EOF, true-RST, body-reset, initial pre-enqueue death, stable semantic-error, and slow-stdio/non-SSH tests pass. The installed fake adapter performs a delayed `SO_LINGER` reset after the first initialize, accepts a second connection, and records exactly one replay before completing launch, configurationDone, threads, and terminate with zero residue.

### Cluster 7: The fake adapter emitted an invalid ThreadsResponse

- **Affected row:** `ENV-DAP-TCP-013`
- **Category:** test bug
- **Severity:** medium test-validity defect; no product defect
- **Symptom:** returning `{}` to `threads` caused the installed client to report an invalid DAP response even though the transport journey otherwise completed.
- **Generator:** the fixture asserted request sequencing but did not conform its response body to the protocol schema.
- **Fix:** return `{"threads": []}`.
- **Verification:** the tracked fixture regression and installed request sequence complete without an invalid-response error.

### Cluster 8: The terminal resize oracle depended on sample position

- **Affected row:** `ENV-TERMINAL-008`
- **Category:** test bug
- **Severity:** medium false-negative risk
- **Symptom:** comparing only the initial and final widths could reject a genuine resize that later returned to its original width.
- **Generator:** the oracle encoded one interaction trace rather than the semantic event.
- **Fix:** persist all observed widths and require at least two distinct values, plus the explicit interrupt evidence.
- **Verification:** the installed receipt records `observedColumns: [162, 109]`, `resizeCount: 1`, and `interrupted: true`.

### Cluster 9: Directory-environment authorization occurred after project open

- **Affected rows:** `ENV-TERMINAL-008`, `ENV-TASK-009`
- **Category:** test bug
- **Severity:** high false-negative risk
- **Symptom:** Zed cached the remote directory environment before the synthetic `.envrc` was authorized, so the fixture values were legitimately absent.
- **Generator:** fixture setup did not respect the product's directory-environment lifecycle.
- **Fix:** run `direnv allow` for the exact synthetic directory before opening the installed app.
- **Verification:** terminal and task receipts contain both the exact authorized directory-environment digest and their surface-specific digest.

### Cluster 10: Cleanup observation counted its own ancestry as residue

- **Affected rows:** all installed remote-surface cleanup checks
- **Category:** test bug
- **Severity:** high false-positive risk
- **Symptom:** the remote Python/SSH observer's ancestor argv contained the fixture path and script names, so the census reported the observer itself as leftover product work.
- **Generator:** string matching was not paired with reuse-safe observer ownership exclusion.
- **Fix:** exclude only the observer's PID/start-time ancestor chain; continue reporting any matching sibling or descendant identity.
- **Verification:** the Linux procfs regression distinguishes ancestors from true residue, and the final installed verification reports an empty process-residue set.

## Product Root Cause Disposition

The branch's product change addresses the original incident generator rather than either harness symptom. Environment-bearing remote consumers previously allowed caller values to be serialized into local SSH process argv, and the interactive path lacked a private readiness state before writing a frame to a PTY. The correction makes generic environment-bearing builders fail closed and routes supported consumers through versioned stdin framing; PTY consumers use an explicit no-echo readiness/complete handshake. Unit and live evidence cover ACP, terminals, tasks, shell execution, context stdio, DAP stdio/TCP, platform refusal, package identity, privacy, and cleanup.

## Anomaly Detection

- A generated plan with 18 mapped rows was not itself evidence; the installed journeys found both harness and product defects after component tests were green.
- A tool call beginning is not a project-read proof. The final canary requires completed evidence plus withheld-oracle matching and a terminal response.
- A permission request is not route success. The Claude Mac negative produced `permission_requested`, approved nothing, cleaned up, and was retained as a safety control; Codex Mac supplied the successful local route.
- The independent MultiMinds audit returned invalid zero-of-nine evidence after its outer timeout and was not converted into approval. Exact-diff architecture review is the independent acceptance route for this candidate.

## Blind-Spot Extrapolation

- **Other remote-shell harnesses:** any test that passes structured source or JSON as multiple SSH arguments can reproduce Cluster 1. The regression asserts the concrete safe boundary, and the product tests separately assert no environment values enter generated argv/debug/error text.
- **Other semantic agent oracles:** requiring related evidence to appear in a single message/tool call can reject correct multi-step work or encourage fixtures that merely echo the expected shape. The canary now aggregates only completed evidence while keeping the digest absent from the prompt.
- **Other captured shell environments:** startup hooks may reference functions, aliases, or files that are not represented by a flat environment map. Captured hook variables must not be replayed as if they were self-contained configuration.
- **Other forwarded services:** local forward connectability is not remote service readiness. Callers that attach a protocol immediately after establishing a tunnel need a service-level readiness or stability oracle.
- **Sibling environment consumers:** context, DAP, Vim shell, terminal, task, and exec-in-shell were all inventoried because repairing only ACP would leave the same privacy generator elsewhere.

No additional unfixed blind spot warrants a new row in this release plan. These extrapolations now have executable regressions or installed-journey coverage in the existing rows.

## Process Recommendations

1. Keep installed canaries as executable release artifacts, not ignored `.vibe` scratch.
2. For every security-sensitive cross-process channel, test both the product protocol and the test harness's real OS transport boundary.
3. Treat semantic evidence as an invariant over completed observations, not a transcript-shape snapshot.
4. Continue requiring assembled Mac-local and Mac→Intrepid journeys before private release acceptance; component and marker-only tests cannot substitute.

## Independent Critique

The final exact-candidate architecture review is required before merge. It must trace every
named consumer, capability and failure behavior, secret-absence invariant, PTY
state machine, and cleanup path. The unavailable MultiMinds panel is recorded as
tooling evidence only and is not acceptance evidence.

## Refactor Readiness

The correction is ready to land without a preparatory refactor. The transport
boundary is explicit: generic environment-bearing builders reject the call,
supported noninteractive consumers share the framed stdin protocol, and terminal
and task consumers share the PTY bootstrap. The two executable canaries are
larger than ordinary unit-test helpers, but each owns one coherent assembled
journey and has focused negative fixtures. Splitting them before landing would
increase cross-file state without reducing the current security or lifecycle
risk.

No must-fix maintainability item was found. Nice-to-have extraction of common
content-free process cleanup or receipt helpers should wait for a second real
consumer rather than creating a speculative abstraction in this release. The
mnemex semantic audit was unavailable because this linked worktree has no local
index and the mnemex CLI is absent; the readiness assessment therefore used the
exact diff, caller inventory, tests, test-plan artifacts, and independent source
review only. That limitation does not change the landing decision.
