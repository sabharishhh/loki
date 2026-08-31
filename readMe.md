# Loki

A personal assistant that runs on your Mac. Rust core, SwiftUI menu bar app, memory you own.

Status: Phase 1, the skeleton. See `.agent/PLAN.md`.

## Shape of the project

```
crates/loki-core/   the core. Loop, event stream, ports, provider adapters
crates/loki-ffi/    the C ABI the app links against
crates/loki-cli/    dev harness. Runs the core with no app
app/                the SwiftUI app, a SwiftPM package
scripts/            build-app.sh assembles Loki.app
```

Two halves. The Rust core does the work; the Swift app is a driving adapter over a C ABI.
**The app cannot link until the core is built**, which is why every app target builds the core
first.

## Setup

Needs Rust 1.96 (pinned in `rust-toolchain.toml`) and Xcode 26.

```bash
make core     # build the Rust side
make check    # fmt, clippy, tests, swift build
```

## Running it

A model key is the only credential. Put it in your shell:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
# or
export OPENAI_API_KEY=sk-...
```

Three variables control which provider runs:

| Variable | Effect |
|---|---|
| `ANTHROPIC_API_KEY` | Anthropic key |
| `OPENAI_API_KEY` | OpenAI key |
| `LOKI_PROVIDER` | `anthropic` or `openai`. Only needed when both keys are set |
| `LOKI_MODEL` | Overrides the provider's default model |

With both keys set and no `LOKI_PROVIDER`, Anthropic wins. Naming a provider whose key is missing
is reported rather than silently falling back.

Pricing follows the model. `adapters/pricing.rs` holds the published rate for each known model, so
setting `LOKI_MODEL` changes the price too. A model the table does not know is recorded as free,
which under-reports rather than inventing a number. Rates are cached and will drift.

This is all temporary. The key moves to the macOS Keychain in Phase 4.

### The core alone, no UI

Fastest way to test a change to the loop.

```bash
make cli
```

Type a message. `Ctrl-C` interrupts a running turn, `Ctrl-D` quits.
Set `LOKI_TRACE=1` to see every event instead of the plain view.

### The app

```bash
make run
```

Builds `Loki.app` and launches it with your shell's environment, so the key comes through.
Look for the small square in the menu bar, then **Open Loki**.

Note that double-clicking `Loki.app` in Finder will not work yet: a Finder launch inherits no
shell environment, so the app starts with no key and says so. Launch it from a terminal, or use
Xcode, until the Keychain lands.

## Working in Xcode

There is no `.xcodeproj` on purpose. Xcode opens the package directly, which gives the editor,
debugger, previews and Instruments with no generated project file to merge.

```bash
make xcode
```

That builds the core first, then opens `app/` in Xcode. Then, once:

1. **Product > Scheme > Edit Scheme** (or `Cmd-<`).
2. Select **Run** in the left column, then the **Arguments** tab.
3. Under **Environment Variables**, click **+** and add your key: `ANTHROPIC_API_KEY` or
   `OPENAI_API_KEY`. Add `LOKI_PROVIDER` and `LOKI_MODEL` as further rows if you need them.
4. Close. `Cmd-R` now runs the app with the key.

Each row has a checkbox, so you can keep both providers configured and tick whichever you want
without retyping keys.

Xcode stores that in `app/.swiftpm/`, which is gitignored, so your key never reaches the repo.

### Opening the whole repo instead

`make xcode` opens `app/` on its own, which always gives a scheme and keeps Cargo's thousands of
build files out of the sidebar. Opening the whole `loki/` folder works too: Xcode detects
`app/Package.swift` as a nested package and the scheme still comes from it. Pick **LokiApp** in the
scheme selector at the top left before `Cmd-R`.

Either way, these are build output and tooling, not source:

| Folder | What it is |
|---|---|
| `build/` | The assembled `Loki.app` |
| `target/` | Cargo output |
| `.agent/`, `.claude/`, `.agents/` | Working docs and agent skills |

**Clicking `build/Loki.app` in Xcode shows "Failed to create archivableRepresentation".** That is
expected. It is a compiled bundle, not source, and Xcode cannot display it. Nothing is wrong.

### Rust changes need a rebuild first

**Xcode does not know about Cargo.** If you change Rust code, run `make core` (or
`cargo build -p loki-ffi`) before hitting `Cmd-R`, otherwise Xcode links the previous build.

Running from Xcode gives you the real menu bar behaviour with no Dock icon, because
`app/Resources/Info.plist` is embedded into the executable at link time. `Cmd-R` and
`make run` behave the same way.

## Voice

Hold **F** in the composer to dictate, release to stop. A tap still types `f`; only a hold past
350ms starts dictation. Speaking while a turn is running interrupts it.

Transcription is on device via `SpeechAnalyzer`. Audio never leaves the Mac and never crosses the
bridge; the Rust core receives text only. macOS asks for microphone access the first time.

Apple Silicon only. The global `opt+space` hotkey is not built yet, because it needs accessibility
permission and its own onboarding step.

## Everyday commands

```bash
make            # list every target
make check      # what CI runs
make test       # tests only
make fmt        # format Rust
make clean      # remove build output
```

## Documentation

`docs/` is the source of truth for the architecture and is not tracked here.
`.agent/` holds the working plan and decision log, also untracked.
