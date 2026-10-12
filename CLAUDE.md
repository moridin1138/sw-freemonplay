# Freemonplay: notes for Claude

## Keep the docs current

Every change that affects behaviour, options, the protocol or the design updates the docs
in the same change, before handing over a commit:

- **CHANGELOG.md:** add a line under `## [Unreleased]` (Added / Changed / Fixed), written
  for users. Mention when hosts and clients must both be updated (protocol bumps). When
  tagging a release, rename `[Unreleased]` to `[x.y.z] - YYYY-MM-DD`, add a fresh empty
  `[Unreleased]` and update the compare links at the bottom; release.yml publishes that
  section as the release notes.
- **README.md:** the status table, usage, options table and known limitations.
- **ARCHITECTURE.md:** crates and files, protocol, threads, zero copy, sessions, pairing.
- **BACKLOG.md:** move finished items out (into the changelog), add new ideas.

## Working on this repo

- rustup comes from Homebrew: `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"`.
- Windows client cross-build: `cargo build --release -p freemonplay-screen --target x86_64-pc-windows-gnu`.
- Linux client: `./build/linux/build.sh` (Docker).
- Mac host app: `./build/macos/bundle.sh --install`.
- Bump `PROTOCOL_VERSION` in `crates/proto/src/lib.rs` whenever a message changes shape.
- Before handing over: `cargo clippy --workspace --all-targets`, `cargo test --workspace`,
  the Windows cross-build, and `freemonplay-screen --self-test` when decoding or
  rendering changed.
- The user commits and pushes; prepare the commit and give them the commands.
- Writing style: no em-dashes or en-dashes; metric units.
