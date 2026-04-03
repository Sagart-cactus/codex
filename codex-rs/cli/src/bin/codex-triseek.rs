#[allow(dead_code)]
#[path = "../main.rs"]
mod shared_cli;

fn main() -> anyhow::Result<()> {
    shared_cli::run_with_startup(shared_cli::CliStartup::triseek())
}
