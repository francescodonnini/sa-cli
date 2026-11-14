mod build;
mod common;
mod conf;
mod doctor;
mod parsers;
mod program;
mod report;
mod test;
mod toolchain;
mod verify;

use crate::build::BuildListParser;
use crate::common::NonEmptySet;
use crate::conf::BuildConf;
use crate::toolchain::Toolchain;
use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use std::fmt::Display;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    DryRun,
    Quiet,
    Verbose,
}

#[derive(Parser)]
#[command(
    version,
    about = "Build, test and profile programs protected by the shadow stack solution."
)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    dry_run: bool,
    #[arg(short, long, global = true)]
    verbose: bool,
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    fn mode(&self) -> RunMode {
        match (self.dry_run, self.verbose) {
            (true, _) => RunMode::DryRun,
            (false, false) => RunMode::Quiet,
            (false, true) => RunMode::Verbose,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    #[command(arg_required_else_help = true)]
    Bench {
        program: String,
        #[arg(value_name = "CONFS", value_parser = BuildListParser)]
        builds: NonEmptySet<BuildConf>,
    },
    #[command(arg_required_else_help = true)]
    Build {
        program: String,
        #[arg(value_name = "CONFS", value_parser = BuildListParser)]
        builds: NonEmptySet<BuildConf>,
        #[arg(short, long, default_value_t = default_jobs())]
        jobs: usize,
    },
    Doctor {
        #[arg(value_name = "CONFS", default_value = "all", value_parser = BuildListParser)]
        builds: NonEmptySet<BuildConf>,
    },
    Init {
        #[arg(long)]
        force: bool,
    },
    Summary {
        csv: PathBuf,
        #[arg(short, long)]
        reference: Option<BuildConf>,
    },
    #[command(arg_required_else_help = true)]
    Test {
        program: String,
        #[arg(value_name = "CONFS", value_parser = BuildListParser)]
        builds: NonEmptySet<BuildConf>,
        #[arg(short, long = "suite", value_name = "SUITES")]
        suites: Option<NonEmptySet<String>>,
        #[arg(short, long, default_value_t = default_jobs())]
        jobs: usize,
    },
}

fn default_jobs() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

impl Display for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::Bench { .. } => write!(f, "bench"),
            Command::Build { .. } => write!(f, "build"),
            Command::Doctor { .. } => write!(f, "doctor"),
            Command::Init { .. } => write!(f, "init"),
            Command::Summary { .. } => write!(f, "summary"),
            Command::Test { .. } => write!(f, "test"),
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let mode = cli.mode();
    // init viene eseguito prima di qualunque altra cosa perché gli altri comandi hanno bisogno di
    // leggere il file toolchain.toml mentre init deve crearlo.
    let toolchain_file = cli.config.unwrap_or_else(default_config);
    if let Command::Init { force } = cli.command {
        return toolchain::init(&toolchain_file, force);
    }

    let tc = Toolchain::load(&toolchain_file)?;
    match cli.command {
        Command::Build {
            program,
            builds,
            jobs,
        } => build::run(&tc, &program, &builds, jobs, mode),
        Command::Doctor { builds } => doctor::run(&tc, &builds, true),
        Command::Init { .. } => unreachable!("init command should've already been handled"),
        Command::Test {
            program,
            builds,
            suites,
            jobs,
        } => test::run(&tc, &program, &builds, suites.as_ref(), jobs, mode),
        _ => bail!("{} not implemented yet", cli.command),
    }
}

/// Ricava il percorso di default dove salvare il file toolchain.toml necessario per ricavare alcuni
/// percorsi utilizzati durante i processi di build, benchmarking e profiling. Per esempio, è
/// necessario per ricavare la posizione di `clang` o della cartella dei sorgenti. Il percorso di
/// default è ~/.config/btp/toolchain.toml.
fn default_config() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("btp/toolchain.toml")
}

#[test]
fn suite_list() {
    let cli = Cli::try_parse_from(["btp", "test", "sqlite", "func", "-s", "a,b,c"]).unwrap();
    let Command::Test { suites: Some(s), .. } = cli.command else { panic!("expected test with suites") };
    assert_eq!(s.iter().map(String::as_str).collect::<Vec<_>>(), ["a", "b", "c"]);
}