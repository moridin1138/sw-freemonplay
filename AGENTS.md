# Notes for coding assistants

Freemonplay turns other computers into extra screens (or keyboard-and-mouse targets) for a
Mac or Windows PC: **Freemonplay Host** makes virtual displays and streams them; **Freemonplay
Screen** shows them. Start with [ARCHITECTURE.md](ARCHITECTURE.md) (crates and files,
protocol, threads, zero copy, sessions, pairing) and [README.md](README.md).

## Working with the maintainer

- **Don't commit, push or tag.** The maintainer does that, in GitHub Desktop. Prepare the
  change and hand over a suggested commit message and the commands. No AI co-author lines
  in commit messages.
- **Writing style** (comments, docs, app text): no em dashes or en dashes; metric units;
  plain words. Keep app text short.
- **Text people see** goes in `locales/en.yml` (`screen.*` and `host.*` keys, used with
  `t!(...)`), with translations in the other files in `locales/`; `cargo test` checks them.
  Logs stay in English, and never record what people type.

## Keep the docs current

Every change that affects behaviour, options, the protocol or the design updates the docs
in the same change:

- **CHANGELOG.md:** a line under `## [Unreleased]` (Added / Changed / Fixed), written for
  users. Say when hosts and screens must both be updated (protocol bumps).
- **README.md:** the status table, usage, options table and known limitations.
- **ARCHITECTURE.md:** crates and files, protocol, threads, zero copy, sessions, pairing.
- **BACKLOG.md:** move finished items out (into the changelog), add new ideas.

Bump `PROTOCOL_VERSION` in `crates/proto/src/lib.rs` whenever a message changes shape.

## Checks before handing over

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"   # rustup comes from Homebrew
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release -p freemonplay-screen --target x86_64-pc-windows-gnu   # Windows cross-build
```

Also `freemonplay-screen --self-test` when decoding or rendering changed. Other builds:
Linux screen `./build/linux/build.sh` (Docker); Mac host app `./build/macos/bundle.sh --install`.

## Releasing

1. Rename `## [Unreleased]` in CHANGELOG.md to `## [x.y.z] - YYYY-MM-DD`, add a fresh empty
   `## [Unreleased]`, update the compare links at the bottom, and set the version in the
   root `Cargo.toml`. Use a minor version for new features or a protocol change.
2. After CI passes on that commit, the maintainer pushes the tag `vx.y.z`.
   **The release workflow publishes the release straight away** (no draft), with that
   changelog section as the notes, so test the builds before the tag is pushed.

`main` is protected (changes are meant to come through pull requests); the maintainer's
pushes bypass that rule, and GitHub says so in the push output.

## Testing safely

- **Don't disturb the maintainer's own host.** For pipeline tests, run a separate one:
  `freemonplay-host --no-menubar --no-mdns --test-pattern --port 47990 --config-dir <tmp>`,
  and connect `freemonplay-screen --host 127.0.0.1:47990 --code <pairing code> --windowed 1600x900 --config-dir <tmp>`.
  `--test-pattern` makes no virtual display and needs no permissions. Freezing the screen
  app (`kill -STOP`) simulates a screen that stops answering. A plain test picture
  compresses to almost nothing; add noise to it locally (don't commit that) to fill the
  network as a real desktop does.
- On a Mac, a real virtual display (`CGVirtualDisplay`) is identified by vendor, product
  and serial number: macOS refuses a second one with the same identity while the first is
  still plugged in.
- Host log on a Mac: `~/Library/Logs/freemonplay/host.log` (the screen app's is `client.log`).
- **Windows:** CI builds with MSVC and the C runtime built in (`.cargo/config.toml`), and
  checks the programs don't need `VCRUNTIME140.dll`. Test on the PC's own screen, not over
  Remote Desktop. Programs started over SSH don't appear on the desktop; start them with a
  temporary interactive scheduled task and remove it afterwards.
- Signing credentials (see SIGNING.md) are private. Never commit or print them.
