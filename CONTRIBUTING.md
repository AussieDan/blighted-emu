# Contributing

Thank you for your help with the emulator.

## Branches

- Make all contributions to the `dev` branch. Open pull requests against `dev`.
- Do not open pull requests against `main`. `main` changes only when `dev` is merged into it.
- Make a branch from `dev` for your work, for example `feature/<name>` or `fix/<name>`.

## Before a pull request

Run the full check and make sure that it passes:

```bash
just check
```

This runs the game data guard, `cargo fmt`, `cargo clippy`, the workspace tests, `cargo deny` and
the script and web tests.

## Commits

- Use the form `<type>(<scope>): <subject>`. The types are `feat`, `fix`, `docs`, `style`,
  `refactor`, `test`, `chore` and `perf`.
- Write the subject in the imperative, with 50 characters or fewer and no closing period.
- Add a body for a larger change. Wrap it at 72 characters.
- Keep each commit to one change.

## Rules

Read the [rules for contributors](README.md#rules-for-contributors) in the README. In short: never
commit game files, captures or secrets, and never connect a changed client to Amazon's servers.
