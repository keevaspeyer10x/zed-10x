# Zed 10x Remote Execution Release Test Plan

Status: execution reopened on 2026-08-27 for exhaustive selectable-variant
coverage. Seventeen rows retain valid evidence; `ACP-JOURNEY-002` awaits the
every-entry route matrix and `ACP-ASSEMBLED-019` awaits installed-app proof for
the five distinct production launch implementations.

Scope: installed Zed 10x external-agent and remote-execution journeys across a
Mac-local project and a Mac-to-Intrepid POSIX SSH project. This is not a claim
about every unrelated upstream Zed feature.

The prior plan had six ACP-only rows. Live cleanup inspection found that a
restored remote terminal exposed the resolved project environment in local SSH
process arguments. Fresh discovery therefore expanded the plan to 19 rows and
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

The picker is independently selectable behavior, not one interchangeable ACP
surface. The Mac-local matrix therefore runs all 12 healthy advertised entries
and the Intrepid matrix runs all 16 exactly once. Each entry must
either complete the withheld-oracle project journey, stop at a safely refused
interactive permission request, or produce an explicit authentication,
capacity/rate-limit, or unsupported-route classification.
Product-origin launch, protocol, cwd, cleanup, inventory, or packaging failures
block the row. The checked inventory must project exactly from the current
canonical managed-agent manifest before any entry starts.

## Canonical rows

| ID | Risk | Executable proof |
|---|---:|---|
| ACP-SMOKE-001 | critical | ordering, reconnect, stale suppression, installed inventory |
| ACP-JOURNEY-002 | critical | all 12 Mac-local and 16 Intrepid healthy picker entries, each with a project-aware terminal result or explicit external-readiness classification |
| ACP-ASSEMBLED-019 | critical | installed Zed picker and `session/new` for Mac custom, Mac registry, Intrepid local, Intrepid persistent and Intrepid registry representatives; other entries require exact route evidence plus mechanical class equivalence |
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
| ENV-PACKAGE-018 | critical | debug bundle, canonical user entry point/process identity, remote capability, rollback |

## Adversarial layer

The run must exercise partial reads, Unicode, quotes, newlines, invalid names,
NULs, malformed lengths, truncation, oversize input, missing capability, early
child exit, PTY echo/canonical buffering, old remote server, cancellation,
timeout, PID residue, a stale same-identifier local app bundle, and a
known-leaking negative-control command. Secrets or
sentinel values are compared by hash/boolean only during live process and log
inspection; the audit must never print them.

## Decision

The release remains `not_ready` until all 19 rows are implemented, executed,
and mapped to durable evidence; independent exact-candidate review is green;
the installed app and matching Intrepid server pass every applicable journey;
required CI passes; and protected merge plus post-merge installed verification
complete.
