# Zed 10x Remote Execution Release Test Plan

Status: freshly regenerated from the `55fd544d` release candidate plus the
uncommitted secure-environment correction on 2026-08-23.

Scope: installed Zed 10x external-agent and remote-execution journeys across a
Mac-local project and a Mac-to-Intrepid POSIX SSH project. This is not a claim
about every unrelated upstream Zed feature.

The prior plan had six ACP-only rows. Live cleanup inspection found that a
restored remote terminal exposed the resolved project environment in local SSH
process arguments. Fresh discovery therefore expanded the plan to 18 rows and
the complete terminal, task, shell, ACP, MCP, DAP stdio, DAP TCP, packaging,
privacy, failure, and cleanup surface.

## Release invariants

1. Caller-supplied remote environment values never appear in a local program
   path, process argument, command debug/error text, terminal echo, or log.
2. Environment-bearing commands use a version-matched, bounded stdin frame.
   Unsupported capability, malformed input, timeout, or unsupported backend
   fails closed without fallback.
3. The private frame is consumed before the first application byte. PTY input
   is not written until the remote side has disabled echo and announced
   readiness.
4. Existing cwd, directory environment, configured terminal environment,
   task-specific environment, protocol, port-forward, input, and cancellation
   behavior remains intact.
5. Completion, cancellation, and timeout leave no owned process residue.

## Surface and journey closure

- Mac-local installed app: ACP inventory/project-read/failure/cleanup journeys.
- Mac app to Intrepid: ACP, terminal, task, Vim shell, context MCP, DAP stdio,
  DAP TCP, capability upgrade, privacy/failure, and cleanup journeys.
- Windows SSH, WSL, and Docker environment-bearing launches: explicit
  fail-closed contract until an equivalent private transport is implemented.

The complete 30-cell matrix, including reasoned non-applicable cells, is in
`docs/discovery-ir.json` and `docs/test-plan.json`. Passing a component test or
a sibling surface cannot satisfy an installed-product cell.

## Canonical rows

| ID | Risk | Executable proof |
|---|---:|---|
| ACP-SMOKE-001 | critical | ordering, reconnect, stale suppression, installed inventory |
| ACP-JOURNEY-002 | critical | installed Mac-local and Intrepid project-aware tool calls |
| ACP-NEG-003 | high | missing executable, auth, capacity, permission, timeout |
| ACP-OPS-004 | high | normal and timeout cleanup |
| ENV-FRAME-005 | critical | round-trip, partial read, malformed and boundary input |
| ENV-SSH-006 | critical | argv/Debug/error privacy, empty-env compatibility, home anchor |
| ENV-PTY-007 | critical | real PTY no-echo handshake, tty restore, first input, early exit |
| ENV-TERMINAL-008 | critical | installed terminal cwd/env/input/resize/Ctrl-C/cleanup |
| ENV-TASK-009 | critical | installed task env/toolchain/cancel/cleanup |
| ENV-SHELL-010 | high | Vim shell prelude, cwd/env, selected input/output |
| ENV-MCP-011 | critical | MCP prelude before initialize, request, cleanup |
| ENV-DAP-STDIO-012 | critical | stdio DAP prelude before initialize, cleanup |
| ENV-DAP-TCP-013 | critical | TCP DAP prelude, port forward, initialize, cleanup |
| ENV-CONSUMER-MATRIX-014 | critical | complete caller inventory and affected crate matrix |
| ENV-CAPABILITY-015 | critical | capability negotiation, version drift, no fallback |
| ENV-PRIVACY-016 | critical | content-free installed argv/log audit with negative control |
| ENV-PLATFORM-017 | high | Windows SSH, WSL, Docker fail closed |
| ENV-PACKAGE-018 | critical | debug bundle, installed identity, remote capability, rollback |

## Adversarial layer

The run must exercise partial reads, Unicode, quotes, newlines, invalid names,
NULs, malformed lengths, truncation, oversize input, missing capability, early
child exit, PTY echo/canonical buffering, old remote server, cancellation,
timeout, PID residue, and a known-leaking negative-control command. Secrets or
sentinel values are compared by hash/boolean only during live process and log
inspection; the audit must never print them.

## Decision

The release remains `not_ready` until all 18 rows are implemented, executed,
and mapped to durable evidence; independent exact-candidate review is green;
the installed app and matching Intrepid server pass every applicable journey;
required CI passes; and protected merge plus post-merge installed verification
complete.
