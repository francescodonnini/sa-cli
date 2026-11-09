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
use crate::conf::{BuildConf, BuildList};
use crate::toolchain::Toolchain;
use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
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
    about = "Build, test and benchmark programs protected by the shadow stack solution."
)]
struct Cli {
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
    Doctor {
        #[arg(value_name = "CONFS", default_value = "all", value_parser = BuildListParser)]
        builds: BuildList,
    },
    #[command(arg_required_else_help = true)]
    Build {
        program: String,
        #[arg(value_name = "CONFS", value_parser = BuildListParser)]
        builds: BuildList,
        #[arg(short, long, default_value_t = default_jobs())]
        jobs: usize,
    },
    #[command(arg_required_else_help = true)]
    Test {
        program: String,
        #[arg(value_name = "CONFS", value_parser = BuildListParser)]
        builds: BuildList,
        #[arg(short, long, default_value_t = default_jobs())]
        jobs: usize,
    },
    #[command(arg_required_else_help = true)]
    Bench { program: String, builds: BuildList },
    Summary {
        csv: PathBuf,
        #[arg(short, long)]
        reference: Option<BuildConf>,
    },
}

fn default_jobs() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
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
    let tc = Toolchain::from_env();
    let mode = cli.mode();
    match cli.command {
        Command::Doctor { builds } => doctor::run(&tc, &builds, true),
        Command::Summary { .. } => bail!("not implemented yet"),
        Command::Build {
            program,
            builds,
            jobs,
        } => {
            build::run(&tc, &program, &builds, jobs, mode)
        }
        Command::Test { program, builds, jobs } => {
            test::run(&tc, &program, &builds, jobs, mode)
        }
        Command::Bench { .. } => bail!("not implemented yet"),
    }
}
