pub mod cli;
pub mod jobs;
pub mod kernel;
pub mod plugins;
pub mod scan;
pub mod server;
pub mod sidecar;

// Domain submodules re-exported so `crate::<submodule>` continues to work directly across the codebase
pub use cli::{conformance, format};
pub use jobs::{agent, catalog, findings, history, record};
pub use kernel::{canon_yaml, frontmatter, jsjson, mask, model, norm, scope, store, tokens, validate, walk};
pub use plugins::{config, enrich};
pub use scan::{analyze, diagnose, extract, provider, resolve};
pub use server::serve;
pub use sidecar::{bump, issues, orphans, sidecars};

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
                cli::CliError::Raw(m, _) => {
                    if !m.is_empty() {
                        eprintln!("{m}");
                    }
                }
                _ => eprintln!("✕  {}", e.message()),
            }
            e.exit_code()
        }
    };
    let prints_elapsed = !matches!(
        cli.command,
        Some(cli::Command::Version | cli::Command::Watch { .. } | cli::Command::Scan { watch: true, .. }
            | cli::Command::Serve(_)
            | cli::Command::Config(config::ConfigArgs {
                command: config::ConfigCommand::List { .. }
                    | config::ConfigCommand::Get { .. }
                    | config::ConfigCommand::Show { .. }
            })
        ) | None
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
