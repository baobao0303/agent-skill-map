mod analyze;
mod canon_yaml;
mod cli;
mod diagnose;
mod extract;
mod format;
mod frontmatter;
mod jsjson;
mod mask;
mod model;
mod norm;
mod provider;
mod resolve;
mod scan;
mod scope;
mod store;
mod tokens;
mod validate;
mod walk;

use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    let started = std::time::Instant::now();
    let cli = cli::Cli::parse();
    let code = match cli::dispatch(&cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("✕  {}", e.message());
            e.exit_code()
        }
    };
    let prints_elapsed = !matches!(
        cli.command,
        Some(cli::Command::Version | cli::Command::Watch { .. } | cli::Command::Scan { watch: true, .. }) | None
    );
    if prints_elapsed && !cli.global.quiet {
        eprint!("\ndone in {}\n", cli::format_elapsed(started.elapsed().as_millis() as u64));
    }
    ExitCode::from(code)
}
