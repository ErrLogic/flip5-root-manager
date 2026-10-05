# Flip5 Root Manager

A local Rust TUI that orchestrates the known, user-provided ADB rooting
workflow for a specific Samsung Galaxy Z Flip5 (`SM-F731B`,
`BP4A.251205.006.F731BXXS7GZG1`). It is an **orchestration tool** around
artifacts built by a separate, read-only payload repository — it does not
implement, modify, or develop the underlying exploit itself. See
[`CLAUDE.md`](CLAUDE.md) for the full specification.

---

## ⚠️ Build the payload first

This app only _runs_ artifacts — it never builds them. Before you run a
**Real Run** workflow, the payload repository must already have a build
for the target (`b5q-F731BXXS7GZG1`) under `build/b5q-F731BXXS7GZG1/`.

If you've pulled new changes into `Root-My-Galaxy-Payloads` (or are
setting things up for the first time), rebuild the payload **before**
launching `flip5-root-manager`:

```sh
cd ../Root-My-Galaxy-Payloads
make TARGET=b5q-F731BXXS7GZG1 ANDROID_NDK_HOME=/path/to/android-ndk

// e.g. make TARGET=b5q-F731BXXS7GZG1 ANDROID_NDK_HOME=~/android-ndk-r29
```

- `ANDROID_NDK_HOME` must point at an NDK containing
  `toolchains/llvm/prebuilt/linux-x86_64/...` (NDK r29 is what the
  payload repo's own docs build against).
- This produces:
    ```text
    build/b5q-F731BXXS7GZG1/cve-2026-43499-app.so
    build/b5q-F731BXXS7GZG1/cve-2026-43499-root
    ```
    which `flip5-root-manager`'s Artifacts panel reads directly — no
    copying needed, as long as the two repos sit side by side (see
    [Workspace layout](#workspace-layout)).
- The KernelSU artifact (`kernelsu/ksud-b5q-F731BXXS7GZG1-kdp`) is
  checked into the payload repo already and does not need building.

`flip5-root-manager` never writes to `Root-My-Galaxy-Payloads` — rebuilding
it is always a manual step you run yourself in that repo.

### Where the `b5q-F731BXXS7GZG1` profile came from

The Flip5 (`b5q-F731BXXS7GZG1`) target profile was added upstream in
[BuSung-dev/Root-My-Galaxy-Payloads#328](https://github.com/BuSung-dev/Root-My-Galaxy-Payloads/pull/328).
The local `Root-My-Galaxy-Payloads` checkout this app points at is the
fork/branch that PR was raised from:
[GiuseppeS0/Root-My-Galaxy-Payloads @ `feat/b5q-F731BXXS7GZG1-profile`](https://github.com/GiuseppeS0/Root-My-Galaxy-Payloads/tree/feat/b5q-F731BXXS7GZG1-profile).

If you need to re-clone or update the payload repo, pull from that
fork/branch (or from upstream once PR #328 is merged) — not an arbitrary
copy — so the Flip5 profile and its exact offsets stay correct.

---

## Workspace layout

`flip5-root-manager` expects the payload repository as a sibling directory
by default:

```text
workspace/
├── Root-My-Galaxy-Payloads/   (read-only artifact source)
└── flip5-root-manager/        (this app)
```

Override the path with `FLIP5_PAYLOAD_REPO=/path/to/Root-My-Galaxy-Payloads`
if your layout differs.

## Building flip5-root-manager

```sh
cargo build --release
```

## Running

```sh
cargo run --release
# or, after building:
./target/release/flip5-root-manager
```

Launching the TUI never starts the workflow by itself — it only probes
ADB/device connectivity for display. You explicitly start the workflow
with `Enter`.

### Environment variables

| Variable                | Purpose                                                                                                                                                                                                              | Default                      |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------- |
| `FLIP5_PAYLOAD_REPO`    | Path to `Root-My-Galaxy-Payloads`                                                                                                                                                                                    | `../Root-My-Galaxy-Payloads` |
| `FLIP5_DRYRUN_SCENARIO` | Dev/test-only Dry Run failure simulation (`payload_failure`, `payload_timeout`, `device_disconnect`, `kernelsu_verification_failure`, `root_verification_failure`, `cleanup_failure`, `invalid_hybrid_mount_config`) | unset = all-success          |

## Modes

- **Real Run** (default) — talks to a real connected device over `adb`.
- **Dry Run** (`D`) — simulates the entire workflow (every state
  transition, retry, cancellation, and log line, each clearly tagged
  `[DRY RUN]`) without ever invoking a real device-changing `adb`
  command. Useful for rehearsing the flow or demoing the TUI without a
  phone attached. Mode can only be switched before a workflow has
  started, and never switches itself.

## Keyboard controls

```text
Enter        Run the next workflow step / confirm artifact selection
D            Toggle Dry Run / Real Run (only before the workflow starts)
R            Retry the current failed step
S            Stop the current running operation
Tab          Switch focus between Terminal Output and Workflow
PageUp/Down  Scroll the focused panel
Home/End     Jump to the top/bottom of the focused panel
C            Clear the terminal output log
Q / Esc      Quit
```

## Workflow

The app walks through the exact reference ADB workflow documented in
[`CLAUDE.md`](CLAUDE.md) section 6, as an explicit state machine (section
7): device detection → verification → payload push/execution → KernelSU
push/stage/load/verification → root verification → cleanup →
`hybrid_mount` inspection/configuration → completed. Every step is
retried only a bounded number of times, every failure is a distinct,
visible state, and real device state (not a cached assumption) is always
re-verified.

## Testing

```sh
cargo fmt --check
cargo check
cargo test
```

Tests run entirely against mocked/simulated ADB backends — no real device
or payload repository write access is required.

## Safety

- The payload repository is treated as strictly read-only: this app never
  modifies, commits to, or writes inside `Root-My-Galaxy-Payloads`.
- No telemetry, analytics, or remote backend of any kind.
- No arbitrary command execution — every `adb` call is one of the fixed,
  documented reference commands in `CLAUDE.md`.
