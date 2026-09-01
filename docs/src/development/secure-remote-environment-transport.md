---
title: Secure Remote Environment Transport
description: Security and lifecycle contract for forwarding remote command environments without exposing values in local process arguments or logs.
---

# Secure Remote Environment Transport

Remote projects resolve environment variables on the execution host and use
them for terminals, tasks, shell commands, external agents, context servers,
and debug adapters. Some values may be credentials. They must never be placed
in the local executable path, process arguments, command debug output, or logs.

## Contract

The generic remote command builder is environment-free. Supplying a non-empty
command environment to it is an error. Environment-bearing commands use a
versioned capability advertised by the version-matched remote server:

- `env-exec-v1` for piped-stdio commands;
- `env-exec-pty-v1` for interactive terminals and tasks.

Both capabilities carry one size-bounded netstring frame on standard input.
The frame contains a JSON object of environment names and values. The remote
server validates the frame completely before executing the requested child.
Malformed, truncated, oversized, unsupported, or timed-out delivery fails
closed. There is no fallback to command-line serialization.

The command template may expose environment variable names and the byte count
of a private prelude for diagnostics. Its debug representation must never
expose prelude bytes or environment values.

## Non-interactive state machine

1. Zed verifies `env-exec-v1` on the installed, version-matched remote server.
2. Zed starts SSH without a pseudo-terminal and invokes `env-exec` from the
   remote user's home directory.
3. Zed writes and flushes the private frame before any ACP, MCP, DAP, or shell
   protocol bytes.
4. The remote server validates the frame, applies the environment, and starts a
   guardian that owns the requested child session and process group.
5. The supervisor retains the sole writer of a private liveness channel. Normal
   transport closure and abrupt supervisor death both close that channel; the
   guardian then kills the provider process group before reaping its leader.

The containment contract covers the requested process and ordinary descendants
that remain in its inherited process group. A provider that deliberately
daemonizes into another session is outside the external-agent process contract.

ACP, shell commands, context-server stdio, DAP stdio, and DAP TCP use this
state machine. For TCP debug adapters the bootstrap input is closed after the
frame is flushed; subsequent debugger traffic uses the forwarded TCP socket.

## Interactive PTY state machine

A normal pseudo-terminal can echo and line-buffer input, so an environment
frame must not be written immediately after SSH starts.

1. Zed verifies `env-exec-pty-v1` on the version-matched remote server.
2. The remote bootstrap disables terminal echo and canonical input and emits a
   unique, content-free readiness marker.
3. Zed withholds the terminal from the UI until it observes that exact marker,
   then writes the private frame through a path that bypasses input logging.
4. The remote server validates the frame, restores the original terminal mode,
   emits the matching completion marker, and replaces itself with the shell or
   task process.
5. Zed observes completion, clears bootstrap markers from the saved terminal
   screen, replays non-terminal events captured during the handshake, and only
   then exposes the terminal to application input.

Missing readiness, child exit, malformed input, or either side's bounded
timeout is a launch failure. Cancellation must terminate the owned SSH and
bootstrap process tree.

## Platform support

POSIX SSH is the supported secure transport. Windows SSH, WSL, and Docker must
fail closed for caller-supplied remote environments until they implement an
equivalent private channel. Internal transport diagnostics such as fixed log
level flags are not caller-supplied remote environments.

## Required verification

Changes to this contract require all of the following:

- round-trip and malformed-frame tests, including partial reads, Unicode,
  quotes, newlines, and the size boundary;
- generated-command tests proving a sentinel value is absent from argv,
  `Debug`, errors, and captured logs;
- a real pseudo-terminal test proving no echo, readiness before delivery,
  terminal-mode restoration, application stdin preservation, and cleanup;
- per-consumer ordering tests proving the prelude precedes ACP, MCP, DAP, or
  shell input;
- a real process-boundary test that kills the `env-exec` supervisor with
  `SIGKILL` and proves a stubborn command leader and descendant do not survive;
- installed Mac-to-remote journeys for terminal, task, shell command, ACP,
  context server, and representative stdio/TCP debug adapters;
- a content-free process/log audit and ordinary cleanup proof.

Update or remove this protocol only when every supported remote transport has
an equivalent channel or a single remote-process API replaces these individual
SSH command launches without weakening the same privacy and lifecycle rules.
