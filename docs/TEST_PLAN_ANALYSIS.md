# Test Plan Analysis — Root Cause Report

> **Date:** 2026-08-27
> **Test run:** complete; all 19 rows have passing, executed, digest-bound evidence
> **Analysis method:** DACI-RP over eight test-harness defects, four assembled-product defects, and two process defects discovered and fixed during deterministic and live execution

## Final readiness disposition

The earlier production-ready conclusion was too strong. `ACP-JOURNEY-002`
sampled one successful Mac route and one successful Intrepid route even though
the picker exposed 14 and 18 independently selectable entries. It also treated
direct ACP protocol execution as if it exercised the installed Zed picker and
launch wrapper. That proved the project-aware oracle and two routes, but it did
not prove the advertised product inventory or assembled user journey. A sibling passing entry is not evidence for another entry
whose executable, package, authentication, host runtime, or protocol path can
fail independently.

The correction adds an exact selectable-variant matrix, binds its inventory to
the canonical managed-agent manifest, executes every advertised route once,
and separates explicit external readiness from product-origin failure. It also
requires the installed picker and `session/new` journey for one representative
of each stable production launch class before mechanically equivalent siblings
can count. The installed Mac and Intrepid pickers exposed exactly 14 and 18
host-labelled routes. All five production execution classes traversed the
installed product; external authentication refusal was classified separately
and a same-class registry control passed. The exact route and assembled-product
receipts are sealed against tested revision `c366ecfec4f19768e42e75597d212a15e9f02022`
and its load-bearing inventory. The decision candidate is production-ready,
subject to exact-head review, protected CI, and a post-merge rebuild/canary.

## Historical analysis retained from the prior run

The prior lifecycle exercised every changed assembled-product surface category through the installed app, but not every independently selectable picker variant. It found seven harness defects and four product defects after the first component-level green. The product's last TCP correction now classifies real disconnect I/O in DAP header and body reads and replays initialize exactly once only for SSH/TCP, within the existing timeout; stdio and non-SSH behavior is unchanged. The final harness corrections made the fake adapter protocol-valid, made resize observation semantic rather than sample-position-dependent, authorized the fixture directory environment before project open, and made cleanup ancestry reuse-safe without hiding a matching non-ancestor. All findings were corrected and re-run. The installed app at product commit `658ad7c7` then completed terminal resize/input/Ctrl-C, task cancellation, Vim filtering, MCP initialization, stdio/TCP DAP request sequences, and two real ACP project reads without exposing private environment values or leaving owned processes. Those two ACP reads remain valid route evidence, but no longer satisfy the full picker row.

## Defect Discovery Value

- **Bugs found:** 14: four product, eight harness, and two process defects
- **Product defects:** 4, all fixed and re-run through the installed app
- **Test harness defects:** 8, all fixed before accepting the UAT
- **Infra/tooling defects:** 0
- **Process defects:** 2, both fixed: representative routes no longer stand in for selectable variants, and direct protocol calls no longer stand in for the assembled picker/session boundary
- **Fixed during run:** SSH remote-command quoting; project-read oracle hardening; resize-sensitive terminal fixture; captured shell-hook sanitization; remote Vim cwd; tagged SSH/TCP initialize replay; valid DAP response fixture; semantic resize oracle; pre-open direnv authorization; reuse-safe cleanup ancestry; exhaustive advertised-route execution; assembled picker execution-class coverage
- **Confidence-only rows:** 0
- **Weak rows:** 0
- **Low-yield assessment:** `coverage_sufficient_after_full_variant_and_assembled_uat`
- **New information learned:**
  - the bundled and uploaded `658ad7c7` remote server advertises and executes both secure environment capabilities;
  - the real SSH PTY consumes a no-echo frame before shell input and preserves cwd, directory/terminal/task environment, stdin, resize, Ctrl-C, and cancellation semantics;
  - the installed Vim filter runs in the real project cwd and transforms selected text;
  - the installed MCP context server completes initialize, initialized notification, and tools/list;
  - the installed stdio and TCP debug adapters complete their real request sequences, and TCP classifies a delayed true reset, opens a second connection, and replays initialize exactly once;
  - a prompt-visible digest or a union of unrelated transcript fragments is not proof of a project read; the final oracle keeps expected bytes private and binds exact path to observed content in one completed read, or uses an exact project-confined ACP client read;
  - the installed Mac and Intrepid pickers expose exactly 14 and 18 host-labelled routes;
  - every advertised route was invoked and classified, and all five stable launch classes crossed the installed picker/session boundary;
  - Codex Mac primary, Intrepid GLM, Intrepid Codex secondary, Intrepid Grok Build, and Intrepid Devin completed exact project-bound controls; the Mac Devin representative stopped only on external authentication.
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
- `docs/test-results/advertised-agent-matrix.json`
- `docs/test-results/installed-picker-uat.json`

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

### Cluster 2: The project oracle confused transcript shape with project access

- **Affected row:** `ACP-JOURNEY-002`
- **Category:** test bug
- **Severity:** high test-validity impact; no product defect
- **Symptom:** the first canary rejected legitimate provider behavior because cwd and digest did not share one call; its interim union-based correction then admitted a false-green shape because the expected digest was prompt-visible and unrelated calls could be combined.
- **5-Why chain:**
  1. A real agent started project tools but the canary reported `project_evidence_mismatch`.
  2. The canary encoded one fixture's transcript shape rather than the user-visible filesystem-read outcome.
  3. The first correction unioned transcript fragments but left the expected digest visible in the prompt.
  4. A fixture could therefore echo or recombine expected values without reading the project file.
  5. The tests checked protocol-shaped text, not whether one read operation bound the exact project path to the actual sentinel bytes.
- **Fix:** keep expected sentinel bytes and digest oracle-side. Pass only when one completed tool call names the exact sentinel path and returns its actual bytes, or when an exact project-confined ACP client read supplies the same content. Refuse all permission requests and retain terminal-marker and cleanup requirements.
- **Verification:** prompt-echo, marker-only, wrong-project, unrelated split-call, permission, auth, capacity, and timeout fixtures remain non-green; the pass fixture actually reads `session_cwd/sentinel.txt`; installed picker representatives read exact local and remote project bytes.

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
- **Fix:** exclude only the observer's exact `(PID, start-time)` ancestor identities, and re-read each scanned process identity around its command line to reject PID reuse; continue reporting any matching sibling or descendant identity.
- **Verification:** the Linux procfs regression keeps a matching sibling process alive, reports that exact sibling PID while excluding the observer ancestry, terminates the sibling, and exits green. The final installed verification reports an empty process-residue set.

### Cluster 11: Selectable variants were collapsed into representative routes

- **Affected row:** `ACP-JOURNEY-002`
- **Category:** process bug
- **Severity:** release-blocking coverage gap
- **Symptom:** one passing Mac route and one passing Intrepid route were treated as coverage for 14 and 18 independently selectable picker entries whose executable, host, package, authentication, and protocol paths can differ.
- **Generator:** the plan described the journey at the feature level but did not close the inventory of user-selectable variants against executed evidence.
- **Fix:** bind the picker inventory to the canonical managed-agent manifest, invoke every advertised entry once without replay, and classify product failures separately from external authentication, capacity, or permission outcomes.
- **Verification:** the sealed route matrix covers exactly 14 Mac and 18 Intrepid entries; omission, staleness, manifest drift, duplicate invocation, and product-origin failure are deterministic negatives.

### Cluster 12: Direct provider calls impersonated the assembled product boundary

- **Affected row:** `ACP-ASSEMBLED-019`
- **Category:** process bug
- **Severity:** release-blocking assembled-system gap
- **Symptom:** a direct ACP client could pass while the installed picker, host projection, launch wrapper, `session/new` call, or Zed-side protocol handling failed.
- **Generator:** protocol-level evidence was accepted as if it traversed the external client's assembled user journey.
- **Fix:** require installed-app evidence for one representative of each mechanically distinct production launch class, while retaining per-route direct evidence for every advertised entry.
- **Verification:** Mac custom, Mac registry, Intrepid local custom, Intrepid persistent, and Intrepid registry classes all crossed the installed picker/session boundary against real project bytes. The standalone official Mac Codex canary mismatch is retained honestly and cannot override the installed-product pass.

## Product Root Cause Disposition

The branch's product change addresses the original incident generator rather than either harness symptom. Environment-bearing remote consumers previously allowed caller values to be serialized into local SSH process argv, and the interactive path lacked a private readiness state before writing a frame to a PTY. The correction makes generic environment-bearing builders fail closed and routes supported consumers through versioned stdin framing; PTY consumers use an explicit no-echo readiness/complete handshake. Unit and live evidence cover ACP, terminals, tasks, shell execution, context stdio, DAP stdio/TCP, platform refusal, package identity, privacy, and cleanup.

## Anomaly Detection

- A generated plan with 18 mapped rows was not itself evidence; the installed journeys found both harness and product defects after component tests were green.
- A tool call beginning is not a project-read proof. The final canary requires completed evidence plus withheld-oracle matching and a terminal response.
- A permission request is not route success. The Claude Mac negative produced `permission_requested`, approved nothing, cleaned up, and was retained as a safety control; Codex Mac supplied the successful local route.
- The independent MultiMinds audit returned invalid zero-of-nine evidence after its outer timeout and was not converted into approval. Exact-diff architecture review is the independent acceptance route for this candidate.

## Blind-Spot Extrapolation

- **Other remote-shell harnesses:** any test that passes structured source or JSON as multiple SSH arguments can reproduce Cluster 1. The regression asserts the concrete safe boundary, and the product tests separately assert no environment values enter generated argv/debug/error text.
- **Other semantic agent oracles:** transcript fields are not outcome evidence. Withhold expected values, bind the real resource and observed result inside one authoritative operation, and keep marker-only, prompt-echo, and unrelated split evidence non-green.
- **Other captured shell environments:** startup hooks may reference functions, aliases, or files that are not represented by a flat environment map. Captured hook variables must not be replayed as if they were self-contained configuration.
- **Other forwarded services:** local forward connectability is not remote service readiness. Callers that attach a protocol immediately after establishing a tunnel need a service-level readiness or stability oracle.
- **Sibling environment consumers:** context, DAP, Vim shell, terminal, task, and exec-in-shell were all inventoried because repairing only ACP would leave the same privacy generator elsewhere.

The newly identified selectable-variant blind spot is now represented inside
`ACP-JOURNEY-002`; it does not need a second duplicate row. Other extrapolations
retain executable regressions or installed-journey coverage in the existing rows.

## Process Recommendations

1. Keep installed canaries as executable release artifacts, not ignored `.vibe` scratch.
2. For every security-sensitive cross-process channel, test both the product protocol and the test harness's real OS transport boundary.
3. Treat semantic evidence as an invariant over completed observations, not a transcript-shape snapshot.
4. Continue requiring assembled Mac-local and Mac→Intrepid journeys before private release acceptance; component and marker-only tests cannot substitute.
5. Treat every independently selectable advertised variant as coverage debt until it has route evidence and direct assembled-product evidence or a mechanically proven equivalence to a directly exercised assembled variant; a provider command or representative sibling is never an implicit substitute.

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
