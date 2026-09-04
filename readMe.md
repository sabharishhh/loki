# Loki

A personal assistant that runs on your Mac. Rust core, SwiftUI menu bar app, memory you own.

Status: Phases 0 to 2 done, the memory core included. Next is either Phase 3 (import, and the
measurement that says whether the memory is any good) or Phase 5 (the web), and that order is
under discussion. See `.agent/PLAN.md` for the chunk list and `docs/Progress.md` for what has
actually been measured.

## Shape of the project

```
crates/loki-core/   the core. Loop, event stream, memory, ports, adapters
crates/loki-ffi/    the C ABI the app links against
crates/loki-cli/    dev harness. Runs the core with no app
app/                the SwiftUI app, a SwiftPM package
scripts/            build-app.sh assembles Loki.app, xcode-prebuild.sh keeps Xcode honest
```

Two halves. The Rust core does the work; the Swift app is a driving adapter over a C ABI.
**The app cannot link until the core is built**, which is why every app target builds the core
first.

### The three rings

`loki-core` is organised in rings, and `tests/rings.rs` enforces the rule rather than trusting it.

| | | |
|---|---|---|
| Ring 0 | `core/`, `memory/` | Locked. The loop, the event stream, the typestate gate, the two-zone prompt |
| Ring 1 | `ports/` | Versioned. `Clock`, `Egress`, `ModelProvider`, `Tool`. A change here needs a version bump and a migration note |
| Ring 2 | `adapters/` | Free to add. Anthropic, OpenAI, the HTTP client, the journal |

Ring 0 and Ring 1 may not name Ring 2, and one adapter may not name another. Both are tested,
including inside `#[cfg(test)]` blocks, because that is where a rule quietly stops holding.

**Every outbound request leaves through `ports::egress`.** `adapters/egress.rs` holds the only HTTP
client in the tree and emits an event before it sends, and a test fails if a second transport
appears anywhere. That is what makes the privacy tier something you can assert against a socket
rather than something enforced by code review. See `tests/egress.rs`.

## Setup

Needs Rust 1.98 (pinned in `rust-toolchain.toml`), Xcode 26, and `cmake` for the TLS build.

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

## Where your data lives

One directory, everything in it plain files or a rebuildable index.

```
~/Library/Application Support/Loki/
  memory/            the store. Markdown with YAML frontmatter, and a git repo of its own
    people/          one file per person
    projects/        one file per project
    preferences/     one file per preference
    episodes/        the permanent dated transcript
    current.md       the session buffer, cleared on close
    working-set.md   generated. What reaches the prompt prefix, capped
    index.md         generated. The catalog a deeper search starts from
    standing.md      standing instructions compaction cannot remove
  index.sqlite       derived from memory/ and disposable. Rebuilt whenever it drifts
  ledger.sqlite      what each model call cost
  loki.log           every prompt, reply, memory event and cost, timestamped
```

`memory/` is a real git repository, so every consolidation is a commit and `git log` inside it is
the history of what Loki learned and when. Nothing here is a proprietary format: a concept file is
a markdown file you can open, edit, and see the edit survive.

The directory does not appear in Finder by default, because `~/Library` is hidden. `Cmd-Shift-G`
in Finder and paste the path, or:

```bash
open ~/Library/Application\ Support/Loki
```

**To start over**, close the app and delete the directory. It is recreated on the next launch, with
the owner and assistant cards seeded before the first turn.

```bash
rm -rf ~/Library/Application\ Support/Loki
```

**To read what happened in a session**, `loki.log` carries every prompt in full. Most defects in
this build were diagnosed from it in minutes.

```bash
tail -f ~/Library/Application\ Support/Loki/loki.log
```

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

Builds `Loki.app` and launches it with your shell's environment, so the key comes through. The
thread window opens straight away, and Loki appears in the Dock while it is open. Close the
window and it drops back to the menu bar with no Dock icon. The square in the menu bar reopens it.

**Keep the key on the same line as the command.** A newline turns it into a shell variable that is
never exported, and the app starts with no key:

```bash
# wrong: the app never sees the key
OPENAI_API_KEY=sk-...
LOKI_PROVIDER=openai ./build/Loki.app/Contents/MacOS/Loki

# right
OPENAI_API_KEY=sk-... LOKI_PROVIDER=openai ./build/Loki.app/Contents/MacOS/Loki

# or export it once
export OPENAI_API_KEY=sk-...
make run
```

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

### Rust changes need a rebuild first, and Xcode will not do it for you

**Xcode does not know about Cargo.** SwiftPM links `libloki_ffi.a` by path with no dependency on
the Rust sources, so building from Xcode alone links whatever core happens to be lying there. The
app runs and every fix you just made is missing, which is worse than not building at all.

`make xcode` builds the core before it opens the project, which covers the first build. For every
build after that, install the pre-action once:

1. **Product > Scheme > Edit Scheme** > **Build** > **Pre-actions** > **+** > New Run Script Action
2. **Provide build settings from:** LokiApp
3. Script: `"$SRCROOT/../scripts/xcode-prebuild.sh"`

Xcode ignores a pre-action's exit code, so the script deletes the linkable archive when the Rust
build fails. A build that will not link is a much better outcome than an app that runs and lies.

Failing that, run `make core` yourself before `Cmd-R`.


Running from Xcode gives you the real menu bar behaviour with no Dock icon, because
`app/Resources/Info.plist` is embedded into the executable at link time. `Cmd-R` and
`make run` behave the same way.

## Voice

Hold **F** in the composer to dictate, release to stop. A tap still types `f`; only a hold past
350ms starts dictation. Speaking while a turn is running interrupts it.

Transcription is on device via `SpeechAnalyzer`. Audio never leaves the Mac and never crosses the
bridge; the Rust core receives text only. macOS asks for microphone access the first time.

Press **opt+space** from any app to bring Loki forward. If another app already owns those keys the
registration is skipped and the menu bar still works.

Apple Silicon only.

## Everyday commands

```bash
make            # list every target
make check      # what CI runs
make test       # tests only
make fmt        # format Rust
make clean      # remove build output
```

## What it can do today

- **Talk, and stream.** Two providers behind one port, interruptible inside 150ms.
- **Remember, and correct itself.** It reads what you tell it into one file per person, project or
  preference, and the same fact said twice does not become two facts. A later statement supersedes
  an earlier one where they cannot both be true, and the earlier one is kept rather than deleted.
- **Show you everything it knows.** The timeline groups what it has learned by what it is about,
  and every row is a line in a file you can open. Nothing is learned invisibly.
- **Search its own memory** when a keyword lookup was not enough, under a hard budget, and say so
  honestly when it does not find something rather than implying you never said it.
- **Account for what it spends**, per turn and per month.

Not yet: the web, actions on your files or calendar, connectors, and running code. Those are
Phases 4 to 6. The key still lives in an environment variable rather than the Keychain.

## Testing and the quality bar

Every commit passes `make check`: format, clippy with warnings denied, the full test suite, and a
Swift build from a clean `.build`.

Two habits are worth knowing if you are contributing, both of which came from defects that tests
had already passed over:

- **Shuffle the cases.** A fixed test set stops testing the system and starts testing itself.
- **Three new edge cases whenever a subsystem is touched**, drawn from a different failure family
  than the last bug, with pruning so the suite stays readable. `.agent/LOG.md` records which cases
  were dropped and why.

A test that has never been seen to fail is a guess. Where a check guards something that matters,
break the fix and watch it fail before you trust it.

## Releasing

`.github/workflows/ci.yml` runs fmt, clippy, tests and a release build on every push, and uploads
the app as an artifact.

The notarization job runs on `main` and skips itself until these repository secrets exist:

| Secret | What |
|---|---|
| `APPLE_CERTIFICATE_P12` | Developer ID Application certificate, base64 encoded |
| `APPLE_CERTIFICATE_PASSWORD` | Password for that `.p12` |
| `APPLE_SIGNING_IDENTITY` | For example `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | Apple ID email |
| `APPLE_TEAM_ID` | Ten character team id |
| `APPLE_APP_PASSWORD` | App-specific password, not your Apple ID password |

`scripts/notarize.sh` does the same thing locally. Entitlements are in
`app/Resources/Loki.entitlements`: microphone for dictation, network for the provider, and
user-selected files.

## Documentation

`docs/` is the source of truth and is not tracked here. It is three files plus a progress report:
`Loki Architecture.md` (the shape, the rings, the failure points), `Loki Memory.md` (how memory
works) and `Loki Subsystems.md` (the web ladder, tools, the interface).

`.agent/` holds the working plan, the decision log and the distilled architecture reference, also
untracked. When the two disagree, `docs/` wins.

