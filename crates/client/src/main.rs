//! Desktop entry point. The iPad app calls the shared library directly.
fn main() -> anyhow::Result<()> {
    freemonplay_screen::run()
}
