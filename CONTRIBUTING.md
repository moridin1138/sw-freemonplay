# Contributing

Thanks for helping. Freemonplay is GPL-3.0; by sending a pull request you agree your change
is released under the same licence.

## Pull requests

1. Fork the repository and make a branch.
2. Make the change. Run `cargo clippy --workspace --all-targets` and
   `cargo test --workspace` (CI runs both on macOS, Windows and Linux).
3. Add a line to `CHANGELOG.md` under `[Unreleased]` if people using the apps will notice.
4. Open a pull request saying what changed and how you tested it.

Every pull request is reviewed before it's merged; the maintainer approves each release.

## Translations

All the text people see, in both apps, lives in the [`locales`](locales) folder, one file
per language: `en.yml` (English, the original), `de.yml`, `es.yml`, `fr.yml`, `tr.yml` and
`zh-CN.yml`. The apps use the computer's language when there's a file for it, and English
otherwise.

**Improving a translation:** edit that language's file and open a pull request.

**Adding a language:**

1. Copy `locales/en.yml` to a file named after the language's code, such as `it.yml`,
   `ja.yml` or `pt-BR.yml` (a region only when it matters, as for Brazilian Portuguese).
2. Translate the text between the quotes. Keep each line's key (before the colon) as it is.
3. Keep placeholders like `%{host}` and `%{count}` exactly as they are; the app fills them in.
4. Keep it short: aim for about the length of the English. Menus and buttons have little room.
5. Leave product names (Freemonplay, Freemonplay Host, Freemonplay Screen), key names
   (Control, Option, Shift) and technical terms like H.264 and HEVC as they are.
6. Run `cargo test --workspace`: it checks every file against `en.yml` (no unknown keys, no
   lost placeholders).

Missing lines fall back to English, so a partial translation is fine to start with.

To see the apps in a language without changing the computer's, set `FMP_LANG`, for example
`FMP_LANG=it freemonplay-screen`.

## Changing the apps' text

New text goes in `locales/en.yml` (keys starting `screen.` for Freemonplay Screen and
`host.` for Freemonplay Host) and is used with `t!("screen.some.key")`. Add the translations
too if you can; otherwise English shows until someone translates it. Log messages stay in
English and aren't translated.
