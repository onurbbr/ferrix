# Ferrix Runtime Architecture

This document describes the stable runtime concepts that are expected to stay
recognizable as Ferrix grows. It is intentionally lightweight; implementation
details may move, but the boundaries below should remain deliberate.

## Runtime Overview

Ferrix is split into small crates:

- `ferrix-core` owns bytecode, values, objects, diagnostics, verification, and
  serialization.
- `ferrix-compiler` turns source packages into verified bytecode programs.
- `ferrix-vm` executes bytecode, owns heap/runtime limits, and calls native
  host functions through explicit capabilities.
- `ferrix-stdlib` registers the built-in native functions.
- `ferrix-runtime` orchestrates source loading, compilation, policy checks,
  execution, daemon state, process history, events, and custom extensions.
- `ferrix-cli` is the command surface. It should stay thin and route runtime
  work through `ferrix-runtime`.

The runtime crate is the boundary for tools. CLI commands, embedding code,
debugger setup, and daemon requests should use runtime request/result types
instead of directly wiring compiler, stdlib, and VM internals.

## CLI To Runtime Flow

Normal source execution follows this path:

```text
ferrix-cli run app.fx
  -> RuntimeGateway
  -> RuntimeController
  -> embedded service or daemon socket
  -> RuntimeService
  -> compiler / bytecode verifier
  -> VM with stdlib and policy capabilities
  -> RunResult + process history + events
```

Inspection commands such as `runtime metrics`, `runtime events`, `runtime
config`, `inspect`, and `explain` should be read-only unless they explicitly
record CLI history.

## Runtime Modes

Ferrix currently has three runtime modes:

- `embedded`: run in the current process. This is the default local mode.
- `required`: require an already-running daemon. User-facing execution fails if
  the runtime is unavailable.
- `managed`: connect to a daemon if it is running, otherwise start one through
  the internal launch path.

`runtime serve` is an internal command. User commands should not need to pass
private flags or environment variables to start it. Runtime launch metadata is
file-based and local to the Ferrix service directory.

## Runtime Profiles

Profiles configure VM limits, audit defaults, and default host capabilities.

- `development`: broad local development permissions.
- `safe`: restricted execution with tighter limits and no default host
  capabilities.
- `cli`: command-line defaults for normal local execution.
- `server`: daemon-oriented defaults with output disabled by default.
- `trusted`: privileged embedding/test profile.

New profile behavior should be documented with the capability changes it
implies.

## Capability Model

Host-visible behavior is guarded by `HostCapability`:

- `native.call`
- `io.output`
- `fs.read`
- `fs.write`
- `env.read`
- `time.read`
- `module.load`
- `extension.call`

Capabilities may be granted by profile defaults or request-level grants. Native
functions and custom extensions should fail through runtime policy errors when
required capabilities are missing.

## Event And Audit Model

Runtime events are typed records retained by the daemon event bus. Events track
lifecycle, process state, debugger activity, policy failures, exceptions,
module loads, GC activity, and custom extension calls.

Audit output is currently returned on requested runs and is also published into
runtime events where useful. Future streaming should build on the same event
vocabulary rather than introducing a second event model.

## Bytecode Container Format

Serialized bytecode is wrapped in a container with metadata:

- bytecode format version
- minimum Ferrix producer version
- required feature flags
- required host capabilities
- entry function
- optional module name
- debug/import/export/interface section markers
- checksum and optimization level

Runtime inspection should use container metadata first and decode the full
program only when execution or structural verification is required.

## Feature Negotiation

The daemon socket protocol starts with `HELLO`. Protocol metadata includes:

- runtime version
- daemon protocol version
- supported protocol range
- stable feature names

The CLI must check protocol compatibility before dispatching daemon-backed
work. Incompatible protocol changes require a compatibility note.

## Custom Extension API

Custom extensions are runtime-owned host operations registered by id. Metadata
declares:

- stable extension id
- human-readable name
- arity
- required capabilities
- estimated cost class
- documentation string

The registry validates existence, arity, `extension.call`, and any additional
capabilities before invoking the host handler.

## Native Function Registry

The standard library installs native functions into the VM. Native callbacks
receive a restricted `NativeContext` so they can allocate objects, inspect heap
values, read runtime limits, and write output only when capability checks pass.

Native functions should declare contract metadata in `ferrix-stdlib` tests when
they become part of the user-visible surface.

## Debugging Model

`ferrix debug` prepares source through `ferrix-runtime`, then runs the VM with
the CLI debugger. The debugger owns interactive state such as breakpoints,
frame selection, disassembly radius, and watches. The VM only sees debugger
actions such as step, continue, or quit.

## Embedding Guide

Embedding code should prefer:

1. Build a `RunSourceRequest`, `RunBytecodeRequest`, or `DebugRequest`.
2. Choose a `RuntimeProfile`.
3. Add explicit capabilities only when needed.
4. Execute through `RuntimeGateway` or `RuntimeService`.
5. Inspect `RunResult`, `RuntimeStats`, and runtime errors by stable category.

Avoid calling CLI internals from embedding code. The CLI is a presentation
layer; `ferrix-runtime` is the integration API.
