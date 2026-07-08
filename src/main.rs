//! The open-source `ragpilot` binary. All logic lives in the library
//! (`ragpilot::run`); this stays a thin entry point so a separate distribution
//! can reuse the same engine via [`ragpilot::run_with_observer`].

fn main() -> anyhow::Result<()> {
    ragpilot::run()
}
