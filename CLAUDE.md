# CLAUDE.md

## Project Overview

This project is a local Rust TUI application for managing a known Samsung Galaxy Z Flip5 device workflow through ADB.

The application is intended to replace the user's current manual terminal workflow with an interactive, state-aware TUI.

The application is an **orchestration and device-management tool** around existing user-provided artifacts.

It is **not** intended to implement, modify, improve, reverse engineer, or bypass the underlying exploit or payload.

The application must preserve the behavior of the user's existing known-good workflow while providing:

- Device detection
- Device compatibility verification
- Artifact selection
- Artifact integrity information
- ADB command execution
- Live stdout/stderr streaming
- Workflow state tracking
- Retry handling
- Timeout handling
- Cancellation
- Device disconnect handling
- KernelSU verification
- Root verification
- Cleanup
- `hybrid_mount` configuration inspection
- Idempotent configuration handling
- Persistent workflow/log state where useful

---

# 1. Development Environment

The application is developed from **WSL**.

Therefore:

- The application must be a terminal application.
- Do not create a GUI.
- Do not introduce desktop GUI frameworks.
- The TUI must work correctly inside WSL.
- The TUI should also work in normal Linux terminals, SSH sessions, and tmux.
- Keyboard-first interaction is preferred.

Recommended stack:

- Rust
- Ratatui
- Crossterm

Keep the dependency tree reasonably small.

---

# 2. Project Separation

The existing payload repository is an external dependency and must be treated as **READ-ONLY**.

Example workspace:

```text
workspace/
├── Root-My-Galaxy-Payloads/
└── flip5-root-manager/
```

The application being developed is:

```text
flip5-root-manager/
```

The payload repository is:

```text
Root-My-Galaxy-Payloads/
```

The application may read artifacts from the payload repository.

The application must never modify the payload repository.

---

# 3. Payload Repository Safety

The payload repository is immutable from the application's perspective.

Never:

- modify source files
- modify build files
- modify exploit code
- modify payload code
- modify generated artifacts
- create commits
- create branches
- checkout another branch
- reset the repository
- rebase
- merge
- pull
- clean
- format
- refactor
- delete files
- overwrite files
- automatically update the repository

The application may inspect:

- repository path
- Git commit
- Git branch
- Git status
- artifact filenames
- artifact sizes
- artifact hashes

Git inspection must be read-only.

If there is any ambiguity about which repository is being modified, stop.

Only the `flip5-root-manager` repository may be changed.

---

# 4. Target Device

The known target device is:

```text
Model:
SM-F731B
```

Expected build:

```text
BP4A.251205.006.F731BXXS7GZG1
```

Known kernel:

```text
5.15.189-android13-8-33404244-abF731BXXS7GZG1
```

The application must verify the device before executing the workflow.

Expected properties:

```text
ro.product.model = SM-F731B

ro.build.display.id = BP4A.251205.006.F731BXXS7GZG1
```

If the model does not match:

```text
DeviceMismatch
```

If the build does not match:

```text
DeviceMismatch
```

The workflow must stop before executing device-changing operations.

Do not bypass compatibility checks.

---

# 5. Existing Artifact Layout

Known payload artifacts:

```text
build/b5q-F731BXXS7GZG1/cve-2026-43499-app.so
build/b5q-F731BXXS7GZG1/cve-2026-43499-root
```

Known KernelSU artifact:

```text
kernelsu/ksud-b5q-F731BXXS7GZG1-kdp
```

The TUI should allow the user to select these artifacts instead of hard-coding absolute paths.

The application should display:

- Artifact name
- Relative path
- Absolute path
- File size
- SHA-256
- Repository location

The application must never silently substitute an artifact.

---

# 6. Reference ADB Workflow

The following commands represent the user's existing manual workflow.

These commands are reference commands for the application.

The implementation should preserve their semantics.

Do not rewrite the underlying payload or exploit behavior.

Where possible, Rust should invoke commands using structured process arguments instead of unsafe shell-string concatenation.

The TUI must display the equivalent command being executed so the user can understand the current operation.

---

## 6.1 Verify Device Model

Reference command:

```bash
adb shell "getprop ro.product.model"
```

Expected result:

```text
SM-F731B
```

Failure condition:

```text
DeviceMismatch
```

---

## 6.2 Verify Device Build

Reference command:

```bash
adb shell "getprop ro.build.display.id"
```

Expected result:

```text
BP4A.251205.006.F731BXXS7GZG1
```

Failure condition:

```text
DeviceMismatch
```

The workflow must not continue if the build does not match.

---

## 6.3 Push Payload Library

Reference command:

```bash
adb push build/b5q-F731BXXS7GZG1/cve-2026-43499-app.so /data/local/tmp/b5q.so
```

The source artifact must come from the selected payload artifact.

Destination:

```text
/data/local/tmp/b5q.so
```

---

## 6.4 Push Payload Runner

Reference command:

```bash
adb push build/b5q-F731BXXS7GZG1/cve-2026-43499-root /data/local/tmp/cve-2026-43499-root
```

Destination:

```text
/data/local/tmp/cve-2026-43499-root
```

---

## 6.5 Make Payload Runner Executable

Reference command:

```bash
adb shell "chmod 755 /data/local/tmp/cve-2026-43499-root"
```

---

## 6.6 Execute Existing Payload Workflow

Reference command:

```bash
adb shell "SLIDE_SOURCE=tracefs EXPLOIT_ATTEMPTS=3 P0_ATTEMPT_TIMEOUT_SEC=115 EXPLOIT_ATTEMPT_TIMEOUT_SEC=600 /data/local/tmp/cve-2026-43499-root --run-payload /data/local/tmp/b5q.so /data/local/tmp/cve-2026-43499-root /data/local/tmp/b5q-fzg1-mcast.log"
```

This is a long-running operation.

The TUI must:

- stream stdout live
- stream stderr live
- show elapsed time
- show current attempt if detectable
- show timeout state
- allow cancellation
- preserve output
- distinguish process failure from timeout
- preserve the final exit status
- allow the user to retry when appropriate

Do not create an infinite retry loop.

The existing configured attempt values must not be silently changed:

```text
SLIDE_SOURCE=tracefs
EXPLOIT_ATTEMPTS=3
P0_ATTEMPT_TIMEOUT_SEC=115
EXPLOIT_ATTEMPT_TIMEOUT_SEC=600
```

---

## 6.7 Push KernelSU Artifact

Reference command:

```bash
adb push kernelsu/ksud-b5q-F731BXXS7GZG1-kdp /data/local/tmp/ksud-s25u-kdp
```

The source must come from the selected KernelSU artifact.

Destination:

```text
/data/local/tmp/ksud-s25u-kdp
```

---

## 6.8 Stage KernelSU

Reference command:

```bash
adb shell "/data/local/tmp/cve-2026-43499-root -c 'cp /data/local/tmp/ksud-s25u-kdp /data/local/tmp/.ksud-stage; chmod 755 /data/local/tmp/.ksud-stage'"
```

The TUI must show this as its own workflow stage.

---

## 6.9 Load KernelSU

Reference command:

```bash
adb shell "/data/local/tmp/cve-2026-43499-root -c 'mount -o bind /data/local/tmp/ksud-s25u-kdp /system/bin/logcat; RUST_LOG=info /system/bin/logcat late-load --allow-shell --package-name me.weishu.kernelsu'"
```

The TUI must treat this as a long-running operation.

Output must be streamed live.

The application must not claim that KernelSU is loaded solely because this process returned successfully.

KernelSU must be verified separately.

---

## 6.10 Verify KernelSU

Reference command:

```bash
adb shell "cat /proc/modules | grep kernelsu"
```

Expected behavior:

The output should contain evidence that the KernelSU module is loaded.

If no KernelSU entry is present:

```text
KernelSU verification failed
```

Do not mark the workflow as successful.

---

## 6.11 Verify Root

Reference command:

```bash
adb shell "su -c 'id'"
```

Expected behavior:

The result must indicate root privileges.

The TUI should display the returned identity information.

Do not consider root verification successful based only on the command exit code if the returned identity does not demonstrate the expected privilege.

---

## 6.12 Cleanup Temporary Mount

Reference command:

```bash
adb shell "su -c 'umount /system/bin/logcat'"
```

Cleanup must be represented as an explicit workflow step.

If cleanup fails:

```text
CleanupFailed
```

Do not silently ignore cleanup failures.

The final workflow state must clearly indicate that cleanup was unsuccessful.

---

## 6.13 Inspect hybrid_mount Configuration

Reference command:

```bash
adb shell su -c "tail -n 6 /data/adb/modules/hybrid_mount/config.toml"
```

The application must inspect the configuration before attempting to modify it.

Expected rule:

```toml
[rules.ViPER4Android-RE]
default_mode = "magic"
```

---

## 6.14 Configure hybrid_mount Rule

The user's existing manual command is:

```bash
adb shell 'su -c '\''awk "{print} /^\\[rules\]\$/ {print "[rules.ViPER4Android-RE]\ndefault_mode = \"magic\"" }" /data/adb/modules/hybrid_mount/config.toml > /data/local/tmp/config.tmp && mv /data/local/tmp/config.tmp /data/adb/modules/hybrid_mount/config.toml'\'''
```

The resulting configuration must contain:

```toml
[rules.ViPER4Android-RE]
default_mode = "magic"
```

The operation must be idempotent.

If the rule already exists with the expected value, do not add another copy.

Do not blindly append the rule every time the application runs.

Prefer using a TOML parser for configuration manipulation instead of reproducing shell text manipulation in Rust.

If the configuration is malformed, stop and report:

```text
ConfigurationInvalid
```

Do not overwrite a malformed configuration automatically.

---

# 7. Workflow State Machine

The workflow must be modeled as an explicit state machine.

Primary states:

```text
Disconnected
DeviceDetected
DeviceVerified
PayloadSelected
PayloadPushed
PayloadExecuting
PayloadSucceeded
KernelSUSelected
KernelSUPushed
KernelSUStaged
KernelSULoaded
KernelSUVerified
RootVerified
TemporaryMountCleaned
HybridMountChecked
HybridMountConfigured
Completed
```

Failure states:

```text
DeviceMismatch
DeviceDisconnected
PayloadFailure
Timeout
VerificationFailed
CleanupFailed
ConfigurationInvalid
UserCancelled
```

The implementation must not rely on a collection of loosely related boolean flags.

Use explicit state transitions.

---

# 8. State Transition Rules

The normal workflow is:

```text
Disconnected
    ↓
DeviceDetected
    ↓
DeviceVerified
    ↓
PayloadSelected
    ↓
PayloadPushed
    ↓
PayloadExecuting
    ↓
PayloadSucceeded
    ↓
KernelSUSelected
    ↓
KernelSUPushed
    ↓
KernelSUStaged
    ↓
KernelSULoaded
    ↓
KernelSUVerified
    ↓
RootVerified
    ↓
TemporaryMountCleaned
    ↓
HybridMountChecked
    ↓
HybridMountConfigured
    ↓
Completed
```

A device mismatch must never transition into payload execution.

A failed verification must never be treated as success.

A cancelled process must transition to:

```text
UserCancelled
```

A disconnected device must transition to:

```text
DeviceDisconnected
```

The user may retry from an appropriate safe state.

---

# 9. TUI Design

The application is a TUI.

The interface should prioritize information density without becoming cluttered.

Recommended layout:

```text
┌──────────────────────────────────────────────────────────────┐
│ Flip5 Root Manager                              Device: OK   │
├──────────────────────────────────────────────────────────────┤
│ Device                                                       │
│ Model : SM-F731B                                             │
│ Build : BP4A.251205.006.F731BXXS7GZG1                       │
│ ADB   : Connected                                             │
├──────────────────────────────────────────────────────────────┤
│ Workflow                                                     │
│ ✓ Device verification                                       │
│ ✓ Payload selection                                          │
│ ✓ Payload push                                               │
│ ▶ Payload execution       Attempt 1/3   00:43               │
│ ○ KernelSU push                                               │
│ ○ KernelSU load                                               │
│ ○ Root verification                                          │
│ ○ Cleanup                                                     │
│ ○ hybrid_mount                                                │
├──────────────────────────────────────────────────────────────┤
│ Terminal Output                                              │
│                                                              │
│ [12:41:22] ...                                               │
│ [12:41:23] ...                                               │
│ [12:41:24] ...                                               │
│                                                              │
├──────────────────────────────────────────────────────────────┤
│ [Enter] Run  [R] Retry  [S] Stop  [Tab] Focus  [Q] Quit     │
└──────────────────────────────────────────────────────────────┘
```

The exact design may evolve.

The information hierarchy must remain clear.

---

# 10. Keyboard Controls

Recommended controls:

```text
↑ / ↓       Navigate
← / →       Change selection where appropriate
Enter       Confirm / execute
Esc         Cancel / go back
R           Retry current failed step
S           Stop current operation
Space       Toggle
Tab         Change focus
PageUp      Scroll logs up
PageDown    Scroll logs down
Home        Log beginning
End         Log end
Q           Quit
```

Do not make critical destructive actions difficult to understand.

The TUI should always indicate the currently focused action.

---

# 11. Terminal Output

Every external command must have observable output.

The terminal panel should support:

- live output
- stdout
- stderr
- timestamps
- auto-follow
- manual scrolling
- clearing
- preserving logs
- command start/end markers
- exit code
- elapsed duration

Example:

```text
[12:41:03] $ adb shell getprop ro.product.model
[12:41:03] stdout: SM-F731B
[12:41:03] exit: 0
[12:41:03] ✓ Device model verified
```

For long-running operations:

```text
[12:41:10] Starting payload
[12:41:10] Attempt 1/3
[12:41:11] ...
[12:41:20] ...
```

Never hide command output merely to make the interface look cleaner.

---

# 12. Command Execution Architecture

Do not scatter raw `Command::new("adb")` calls throughout the application.

Create an abstraction.

Suggested structure:

```text
src/
├── executor/
│   ├── mod.rs
│   └── process.rs
│
└── adb/
    ├── mod.rs
    ├── client.rs
    └── mock.rs
```

The `AdbClient` should provide operations such as:

```text
devices()
shell()
push()
```

The process executor should provide:

- command spawning
- argument handling
- stdout streaming
- stderr streaming
- exit status
- timeout
- cancellation
- process lifecycle

Prefer structured arguments:

```rust
Command::new("adb")
    .args(["shell", "getprop", "ro.product.model"])
```

rather than constructing arbitrary shell strings.

For commands that intentionally require a shell on the Android device, make that shell boundary explicit.

---

# 13. Host vs Device Command Boundary

The application must clearly distinguish:

```text
HOST COMMAND
adb push ...
adb devices
```

from:

```text
DEVICE COMMAND
adb shell ...
```

and from:

```text
ROOT DEVICE COMMAND
adb shell su -c ...
```

The TUI should display this distinction in logs.

Never accidentally execute an Android command on the WSL host.

Never accidentally execute a host command through the Android shell.

---

# 14. Retry Policy

Retries must be explicit and bounded.

Never implement:

```text
while !success {
    retry();
}
```

without a hard limit.

The payload workflow already has its own configured attempt behavior.

The TUI-level retry mechanism should be separate from the payload's internal attempts.

For example:

```text
Workflow retry:
    User chooses Retry

Payload internal attempts:
    EXPLOIT_ATTEMPTS=3
```

Do not silently multiply retries.

Every retry must be visible in the TUI.

---

# 15. Timeout Handling

Long-running commands must have explicit timeout behavior.

When a timeout occurs:

```text
Timeout
```

must be represented as a distinct state/error.

The TUI should show:

- command
- timeout duration
- elapsed duration
- partial output
- whether the process was terminated
- whether retry is available

Never freeze the TUI while waiting for a process.

---

# 16. Cancellation

The user must be able to stop a running operation.

Cancellation must:

1. signal the running process;
2. wait for process termination;
3. capture final output;
4. update workflow state;
5. preserve logs.

Cancellation should not leave the TUI believing the process is still running.

---

# 17. Device Disconnect Handling

ADB disconnection is a normal runtime condition.

The application should detect:

```text
DeviceDisconnected
```

and preserve:

- current workflow state
- last successful step
- current step
- command output
- error information

After reconnection:

1. detect the device;
2. verify model;
3. verify build;
4. inspect current state;
5. allow the user to resume or retry an appropriate step.

Do not blindly restart the entire workflow after reconnection.

---

# 18. Reboot Recovery

The device may reboot between sessions.

When the TUI starts:

- do not automatically execute the workflow;
- detect the device;
- verify model/build;
- inspect available state;
- show the user what is currently known;
- let the user choose whether to continue.

The application should not assume that a previous workflow completed simply because a local state file says it did.

The actual device state is authoritative.

---

# 19. Verification

Important operations require actual state verification.

Do not use:

```text
exit code == 0
```

as the only indication of success when a state check exists.

Examples:

KernelSU:

```bash
adb shell "cat /proc/modules | grep kernelsu"
```

Root:

```bash
adb shell "su -c 'id'"
```

Configuration:

```bash
adb shell su -c "tail -n 6 /data/adb/modules/hybrid_mount/config.toml"
```

The workflow should distinguish:

```text
CommandSucceeded
```

from:

```text
StateVerified
```

A command can succeed while the desired state is not present.

---

# 20. Artifact Integrity

Before using an artifact, the TUI should be able to display:

```text
Name
Path
Size
SHA-256
```

Optionally:

```text
Git commit
Git branch
Git status
```

The application must never modify the artifact merely to calculate its checksum.

Checksum calculation must be read-only.

---

# 21. Suggested Source Layout

Recommended project structure:

```text
flip5-root-manager/
├── Cargo.toml
├── Cargo.lock
├── CLAUDE.md
├── README.md
├── workflow/
│   └── flip5.yaml
│
├── src/
│   ├── main.rs
│   ├── app.rs
│   │
│   ├── adb/
│   │   ├── mod.rs
│   │   ├── client.rs
│   │   └── mock.rs
│   │
│   ├── device/
│   │   ├── mod.rs
│   │   ├── detector.rs
│   │   └── verifier.rs
│   │
│   ├── artifacts/
│   │   ├── mod.rs
│   │   ├── discovery.rs
│   │   └── checksum.rs
│   │
│   ├── executor/
│   │   ├── mod.rs
│   │   └── process.rs
│   │
│   ├── workflow/
│   │   ├── mod.rs
│   │   ├── state.rs
│   │   ├── runner.rs
│   │   ├── retry.rs
│   │   └── steps/
│   │
│   ├── terminal/
│   │   ├── mod.rs
│   │   └── buffer.rs
│   │
│   ├── config/
│   │   ├── mod.rs
│   │   └── hybrid_mount.rs
│   │
│   └── tui/
│       ├── mod.rs
│       ├── layout.rs
│       ├── widgets.rs
│       ├── events.rs
│       └── screens/
│
└── tests/
```

This structure is a recommendation, not an absolute requirement.

Do not create unnecessary abstraction layers merely for the sake of abstraction.

---

# 22. Workflow Definition

Where practical, keep workflow metadata separate from Rust implementation.

Example:

```text
workflow/
└── flip5.yaml
```

The workflow definition can describe:

- device requirements
- artifact paths
- destination paths
- step names
- verification commands
- timeout values
- retry policy

However, security-sensitive or complex operations should remain explicit in Rust rather than becoming arbitrary executable configuration.

Do not create a generic scripting engine.

The application is a specific Flip5 workflow manager.

---

# 23. Configuration Handling

For `hybrid_mount`:

1. Read the current configuration.
2. Parse it.
3. Check whether the required rule exists.
4. Check its value.
5. Modify only when necessary.
6. Write the resulting configuration safely.
7. Re-read the configuration.
8. Verify the rule.

Expected rule:

```toml
[rules.ViPER4Android-RE]
default_mode = "magic"
```

The operation must be idempotent.

Running the workflow twice must not result in duplicate configuration blocks.

---

# 24. Persistence

The application may persist:

- last selected payload
- last selected KernelSU artifact
- last payload repository path
- workflow history
- logs
- last known workflow state

Do not treat persisted state as authoritative device state.

Persisted state is only a convenience for recovery and user experience.

The actual Android device must always be re-verified.

---

# 25. Testing

Unit tests should not execute the real payload.

Use mocked ADB/process layers.

Tests should cover:

### Device

- no device
- multiple devices
- correct device
- wrong model
- wrong build
- device disconnect

### Artifact

- artifact discovery
- missing artifact
- checksum
- correct artifact
- incorrect/mismatched artifact

### Workflow

- successful workflow
- retryable failure
- non-retryable failure
- timeout
- cancellation
- disconnect
- verification failure
- cleanup failure

### KernelSU

- loaded
- not loaded
- verification failure

### Root

- root verified
- root unavailable
- unexpected `id` output

### hybrid_mount

- rule exists
- rule missing
- rule duplicated
- malformed TOML
- idempotent update

### Process execution

- stdout
- stderr
- exit code
- timeout
- cancellation

---

# 26. Mock ADB Layer

The TUI and workflow engine should be testable without a physical phone.

Example mock scenarios:

```text
MockDeviceConnected
MockDeviceMismatch
MockBuildMismatch
MockPayloadSuccess
MockPayloadFailure
MockPayloadTimeout
MockKernelSULoadSuccess
MockKernelSULoadFailure
MockRootSuccess
MockRootFailure
MockDeviceDisconnect
MockCleanupFailure
MockInvalidConfig
```

Tests should verify state transitions rather than only individual function outputs.

---

# 27. Error Reporting

Errors should be human-readable.

Bad:

```text
Error: exit code 1
```

Better:

```text
Payload execution failed

Exit code: 1
Elapsed: 42.7s

The payload process exited unsuccessfully.

[R] Retry
[S] Stop
[Esc] Back
```

When possible include:

```text
Command
Step
Exit code
stderr
Relevant stdout
Elapsed time
Suggested action
```

Never hide the original process error.

---

# 28. Logging

Use structured application logging where useful.

Workflow logs should contain:

```text
timestamp
workflow state
step
command
stdout/stderr
exit code
duration
result
```

Do not send logs to a remote service.

No telemetry.

No analytics.

No remote backend.

No automatic uploads.

---

# 29. Security Boundaries

The application operates on a rooted Android device and therefore must be conservative.

Do not:

- execute arbitrary commands entered by remote users;
- expose an HTTP command API;
- add remote control functionality;
- add telemetry;
- upload device information;
- automatically modify unrelated Android files;
- automatically delete files outside the known workflow;
- bypass device compatibility checks;
- hide command output;
- claim success without verification.

Commands should be explicit and associated with known workflow steps.

---

# 30. No Automatic Startup Execution

Launching the TUI must never automatically begin the workflow.

Startup should:

1. initialize the terminal;
2. detect ADB;
3. detect the device;
4. verify compatibility;
5. show the current state;
6. wait for user action.

The user must explicitly start the workflow.

---

# 31. User Experience Principles

The application should feel like a small professional device-management utility.

Priorities:

1. Reliability
2. Transparency
3. Recoverability
4. Clear state
5. Live output
6. Fast interaction
7. Minimal configuration

Avoid unnecessary animations.

Avoid excessive UI decoration.

The user should always know:

```text
What is happening?
Why is it happening?
What command is running?
How long has it been running?
Did it succeed?
What failed?
What can I do next?
```

---

# 32. Important Implementation Principle

Do not build this as:

```text
button -> shell command -> next button -> shell command
```

Build it as:

```text
Device
   ↓
Compatibility
   ↓
Workflow State Machine
   ↓
Step Executor
   ↓
ADB Abstraction
   ↓
Verification
   ↓
Next State
```

The workflow engine is the core of the application.

The TUI is the presentation layer.

ADB is the device transport layer.

The payload repository is an immutable artifact source.

---

# 33. Repository Rules

Before modifying any file:

1. Determine which repository the file belongs to.
2. Confirm it is `flip5-root-manager`.
3. Never modify `Root-My-Galaxy-Payloads`.
4. If repository ownership is ambiguous, stop.

Never use broad commands that could affect both repositories.

Avoid commands such as:

```bash
git clean -fd
git reset --hard
```

unless explicitly requested for the application repository and clearly scoped.

Never execute destructive Git operations against the payload repository.

---

# 34. Forbidden Scope Expansion

Do not expand the project into:

- exploit development
- exploit optimization
- exploit modification
- payload modification
- kernel modification
- bootloader unlocking
- remote rooting service
- cloud backend
- web dashboard
- GUI application
- automatic device fleet management
- arbitrary Android command execution

The project remains:

```text
Local Rust TUI
+
ADB
+
Existing user-provided artifacts
+
Explicit workflow orchestration
+
Verification
```

---

# 35. Definition of Done

The project is considered complete when the user can:

- launch the TUI from WSL;
- detect the connected Flip5;
- verify the model;
- verify the build;
- reject an incompatible device;
- browse/select the payload artifact;
- browse/select the KernelSU artifact;
- see artifact SHA-256;
- execute the workflow;
- see live stdout/stderr;
- see elapsed time;
- see workflow state;
- retry failed steps;
- cancel long-running operations;
- handle device disconnection;
- verify KernelSU;
- verify root;
- clean up the temporary mount;
- inspect `hybrid_mount`;
- apply the required rule idempotently;
- restart the TUI after a reboot;
- inspect/recover workflow state;
- never modify the payload repository.

The application must remain responsive throughout long-running ADB operations.

---

# 36. Engineering Principle

The project has three clearly separated responsibilities:

```text
Payload Repository
    ↓
Read-only artifact source


Flip5 Root Manager
    ↓
Workflow orchestration
TUI
ADB process management
Verification
Logging
Recovery


Android Device
    ↓
Actual execution target
```

The most important architectural rule is:

> The payload repository provides artifacts. The TUI manages the workflow. The device's actual state determines whether the workflow succeeded.
