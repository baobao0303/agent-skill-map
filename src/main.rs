mod analyze;
mod canon_yaml;
mod cli;
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
mod walk;

use clap::Parser;
use std::io::IsTerminal;
use std::process::ExitCode;

fn main() -> ExitCode {
    let started = std::time::Instant::now();
    let cli = cli::Cli::parse();
    let color = !cli.global.no_color && std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal();
    let code = match cli::dispatch(&cli, &cli::Out { color }) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("✕  {}", e.message());
            e.exit_code()
        }
    };
    let prints_elapsed = !matches!(cli.command, Some(cli::Command::Version) | None);
    if prints_elapsed && !cli.global.quiet {
        eprint!("\ndone in {}\n", cli::format_elapsed(started.elapsed().as_millis() as u64));
    }
    ExitCode::from(code)
}
