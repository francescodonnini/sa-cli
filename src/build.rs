use crate::common::{
    Layout, NonEmptySet, Vars, checkout_only, clone, command, expand, run_logged, run_quiet, vars,
};
use crate::conf::BuildConf;
use crate::program::{Program, Source};
use crate::report::IrReport;
use crate::toolchain::Toolchain;
use crate::{RunMode, doctor, verify};
use anyhow::{Context, Error, Result, bail};
use clap::builder::{PossibleValue, TypedValueParser};
use clap::error::ErrorKind;
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Esegue un comando di build per `program`. `tc` viene usato per ricavare la posizione del
/// compilatore (`clang`) e della directory dove salvare gli eseguibili prodotti al completamento
/// del processo (`targets`).
pub fn run(
    tc: &Toolchain,
    program: &str,
    builds: &NonEmptySet<BuildConf>,
    jobs: usize,
    mode: RunMode,
) -> Result<()> {
    if mode != RunMode::DryRun {
        doctor::require(tc, builds)?;
    }
    let prog = Program::load(&tc.programs, program)?;
    if tc.clangxx.is_none() && prog.build_uses("cxx") {
        bail!(
            "{} uses {{cxx}} but no C++ compiler was found next to {} (set clangxx in toolchain.toml)",
            prog.name,
            tc.clang.display()
        );
    }
    for &conf in builds.iter() {
        let dirs = tc.layout(&prog, conf);
        let vars = vars(tc, &prog, conf, &dirs, jobs);
        let mut commands = build_steps(&prog, &vars, &dirs.build)?;
        if conf.needs_ir_plugin() {
            commands.iter_mut().for_each(|c| {
                c.env("SS_REPORT_DEST", &dirs.report);
            });
        }
        if mode == RunMode::DryRun {
            println!("# {} {conf}", prog.name);
            for c in prepare(&prog, &dirs).iter().chain(&commands) {
                println!("{c:?}");
            }
            continue;
        }
        println!("== {} {conf}", prog.name);
        for mut c in prepare(&prog, &dirs) {
            run_quiet(&mut c, mode)?;
        }
        let logs = dirs.meta.join("logs");
        fs::create_dir_all(&logs).with_context(|| format!("cannot create {}", logs.display()))?;
        for (i, c) in commands.iter_mut().enumerate() {
            let log = logs.join(format!("{:02}-{}.log", i + 1, step_name(c)));
            println!("  {}", step_name(c));
            run_logged(c, &log, mode)?;
        }
        install(&prog, &dirs)?;
        for v in &prog.build.verify {
            let path =
                expand(v.path(), &vars).with_context(|| format!("{}: build.verify", prog.name))?;
            let path = dirs.build.join(path);
            println!(
                "  verified {}",
                verify::binary(tc, conf, &path, v.min_refs())?
            );
        }
        if let Some(r) = IrReport::read(&dirs.report)? {
            println!("  {}", r.summary());
        }
        println!(
            "== {} {conf}: ready in {}",
            prog.name,
            dirs.prefix.display()
        );
    }
    Ok(())
}

/// Legge la lista di comandi presente `[steps]` in `[build]` e sostituisce i simboli avvolti
/// da parentesi graffe con i valori corrispondenti riportati in `vars`.
fn build_steps(prog: &Program, vars: &Vars, cwd: &Path) -> Result<Vec<Command>> {
    let mut env = Vec::new();
    if prog.build.export_flags {
        for (name, var) in [
            ("CC", "cc"),
            ("CXX", "cxx"),
            ("CFLAGS", "copt"),
            ("CXXFLAGS", "copt"),
            ("LDFLAGS", "ldopt"),
        ] {
            if let Some(value) = vars.get(var) {
                env.push((name.to_string(), value.clone()));
            }
        }
    }
    for (k, v) in &prog.build.env {
        let v = expand(v, vars).with_context(|| format!("{}: build.env.{k}", prog.name))?;
        env.push((k.clone(), v));
    }
    let mut commands = Vec::new();
    for (i, step) in prog.build.steps.iter().enumerate() {
        let command = command(step, vars, cwd, &env)
            .with_context(|| format!("{}: build.step {}", prog.name, i))?;
        commands.push(command);
    }
    Ok(commands)
}

/// Clona la directory da remoto, ne seleziona la versione specificata nel file toml e se c'è
/// bisogno (`copy_source == true`) la copia nella directory di build.
///
/// E.g.
/// prog = {
///   name="postgres",
///
fn prepare(prog: &Program, dirs: &Layout) -> Vec<Command> {
    let mut v = match &prog.source {
        Source::Git { git, git_ref } => clone(git, git_ref, &dirs.source),
        Source::Path { .. } => Vec::new(),
    };
    let mut c = Command::new("rm");
    c.arg("-rf")
        .arg(&dirs.build)
        .arg(&dirs.prefix)
        .arg(&dirs.meta);
    v.push(c);

    if prog.build.copy_source {
        let Source::Git { git_ref, .. } = &prog.source else {
            unreachable!("Program::load rejects copy_source without a git source");
        };
        let mut c = Command::new("git");
        c.args(["clone", "-q", "--shared", "--no-checkout"])
            .arg(&dirs.source)
            .arg(&dirs.build);
        v.push(c);
        v.push(checkout_only(&dirs.build, git_ref));
    } else {
        let mut c = Command::new("mkdir");
        c.arg("-p").arg(&dirs.build);
        v.push(c);
    }
    v
}

fn step_name(c: &Command) -> String {
    let prog = Path::new(c.get_program())
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let first = c
        .get_args()
        .map(|a| a.to_string_lossy())
        .find(|a| !a.starts_with('-') && !a.contains('='));
    match first {
        Some(a) if !a.contains('/') => format!("{prog}-{a}"),
        _ => prog.to_string(),
    }
}

fn install(prog: &Program, dirs: &Layout) -> Result<()> {
    // prefix esiste sempre a fine build, anche se il programma non installa niente:
    // test lo usa per sapere che l'ultima build è arrivata in fondo
    fs::create_dir_all(&dirs.prefix)
        .with_context(|| format!("cannot create {}", dirs.prefix.display()))?;
    for (from, to) in &prog.build.install {
        let (from, to) = (dirs.build.join(from), dirs.prefix.join(to));
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        fs::copy(&from, &to)
            .with_context(|| format!("cannot install {} to {}", from.display(), to.display()))?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct BuildListParser;

impl TypedValueParser for BuildListParser {
    type Value = NonEmptySet<BuildConf>;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        _arg: Option<&clap::Arg>,
        value: &OsStr,
    ) -> Result<NonEmptySet<BuildConf>, clap::Error> {
        let s = value
            .to_str()
            .ok_or_else(|| clap::Error::new(ErrorKind::InvalidUtf8).with_cmd(cmd))?;
        s.parse().map_err(|e: Error| {
            clap::Error::raw(ErrorKind::InvalidValue, format!("{e}\n")).with_cmd(cmd)
        })
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = PossibleValue> + '_>> {
        let confs = BuildConf::ALL
            .iter()
            .filter_map(clap::ValueEnum::to_possible_value);
        let all = PossibleValue::new("all").help("every configuration");
        Some(Box::new(confs.chain(std::iter::once(all))))
    }
}
