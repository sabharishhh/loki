# Loki

A personal assistant that runs on your Mac, remembers you, and corrects itself when what it
remembers stops being true.

It lives in the menu bar. Your memory lives on your machine, in plain markdown you can open and
edit. The model is rented from whichever provider you point it at and is the replaceable part.

Apple Silicon only.

## What it does today

**Talks, and streams.** Two providers behind one interface, and you can interrupt it mid answer.
It stops inside 150 milliseconds, measured rather than claimed.

**Remembers what you tell it.** One file per person, project or preference. Say something once and
it is usable straight away, and saying the same thing twice does not turn it into two facts.

**Corrects itself.** When two things cannot both be true, the later one is used and the earlier one
is kept rather than deleted. It records both when a fact became true and when Loki found out, so
it can tell you it was wrong about something for six weeks, and for which six weeks.

**Shows you everything it knows.** A single screen, grouped by who or what each fact is about, with
what it is called and what it is connected to. Every row is a line in a file you can open. Nothing
is learned invisibly, and if you disagree with something you can correct it there.

**Searches its own memory** when an ordinary lookup was not enough, under a hard budget so it
cannot run away. If it does not find something it says so, rather than implying you never told it.

**Says when it cannot read something.** If you edit a file by hand and break it, Loki stops using
that card, tells you which file and which line, and does not quietly carry on answering from the
version it remembers.

**Accounts for what it spends**, per turn and per month.

**Listens.** Hold F to dictate and release to stop. Transcription happens on the device, so audio
never leaves your Mac. Speaking while it is answering interrupts it.

**Comes when called.** Press opt and space from any app to bring it forward.

## Memory you own

Everything Loki knows is markdown with a little structure at the top, in a folder on your disk. No
database, no proprietary format, no account.

That folder is a git repository, so every time Loki consolidates what it learned it makes a commit,
and its history is the history of what it learned and when. You can read it, edit it, copy it to
another Mac, or take it somewhere else entirely.

## Building it

`CONTRIBUTING.md` covers setup, running it, working in Xcode, testing and releasing.

## Not yet

The web, actions on your files and calendar, connectors to accounts you already use, and running
code. The model key currently lives in an environment variable rather than the Keychain.
