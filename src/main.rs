mod analyze;
mod bump;
mod canon_yaml;
mod cli;
mod diagnose;
mod extract;
mod format;
mod frontmatter;
mod issues;
mod jsjson;
mod mask;
mod model;
mod norm;
mod orphans;
mod provider;
mod resolve;
mod scan;
mod scope;
mod sidecar;
mod sidecars;
mod store;
mod tokens;
mod validate;
mod walk;

use clap::Parser;
use clap::error::{ContextKind, ContextValue, ErrorKind};
use std::process::ExitCode;

fn main() -> ExitCode {
    let started = std::time::Instant::now();
    let cli = match cli::Cli::try_parse() {
        Ok(c) => c,
        Err(e) => return parse_error(e),
    };
    let code = match cli::dispatch(&cli) {
        Ok(code) => code,
        Err(e) => {
            match &e {
                cli::CliError::Raw(m, _) => eprintln!("{m}"),
                _ => eprintln!("✕  {}", e.message()),
            }
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

/// Unknown verbs / options are reported in the original's words (exit 2);
/// help and version requests keep clap's own rendering.
fn parse_error(e: clap::Error) -> ExitCode {
    let arg = match e.get(ContextKind::InvalidArg).or_else(|| e.get(ContextKind::InvalidSubcommand)) {
        Some(ContextValue::String(s)) => s.split_whitespace().next().unwrap_or("").to_string(),
        _ => String::new(),
    };
    let verb = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    match e.kind() {
        ErrorKind::UnknownArgument => match verb {
            Some(v) => eprintln!("sm: {v}: unknown option '{arg}'"),
            None => eprintln!("sm: unknown option '{arg}'"),
        },
        ErrorKind::InvalidSubcommand => eprintln!("sm: unknown command '{arg}'"),
        _ => e.exit(),
    }
    eprintln!("Run 'sm help' to see the full command list.");
    ExitCode::from(2)
}
