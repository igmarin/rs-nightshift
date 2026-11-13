<!-- "Closes #N" only when this PR finishes the issue; otherwise "Part of #N". -->
Part of #

## What changed

## How it was verified

<!-- The commands you ran and what they showed. For a change to how runs work, the nightshift run you tried. -->

## Checklist (AGENTS.md)

- [ ] One concern; under 150 changed lines, or the reason it is larger
- [ ] Tests written first for new logic; a bug fix has a test that failed before
- [ ] Local gate passes: fmt, clippy, test, doc tests, `cargo doc`, audit, deny
- [ ] Coverage stays at or above 85%
- [ ] No invariant broken (INV-1 to INV-11 in `.github/review-prompt.md`)
- [ ] `CHANGELOG.md` and docs updated for any change a user can see
- [ ] Opened as a draft; marked ready only when CI was green
