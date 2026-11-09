use crate::conf::{BuildConf, BuildList};
use crate::ir_report::IrReport;
use crate::program::Program;
use crate::toolchain::Toolchain;
use crate::{RunMode, doctor, verify};
use anyhow::{bail, Context, Error, Result};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;
use clap::builder::{PossibleValue, TypedValueParser};
use clap::error::ErrorKind;

pub type Vars = BTreeMap<&'static str, String>;

/// Contiene i percorsi usati durante il processo di build di un programma
/// * `build` è il posto in cui viene salvato il programma compilato,
/// * `prefix` è il percorso in cui viene salvata una configurazione,
/// * `report` è il percorso del file in cui il compilatore salva le metriche raccolte dai pass
///    del compilatore,
/// * `source` è il percorso in cui viene salvato il codice sorgente del programma.
pub struct Dirs {
    pub build: PathBuf,
    pub prefix: PathBuf,
    pub report: PathBuf,
    pub source: PathBuf,
}

impl Dirs {
    pub fn new(targets_root: &Path, program: &str, conf: BuildConf) -> Dirs {
        let base = targets_root.join(program);
        Dirs {
            build: base.join(format!("build-{conf}")),
            prefix: base.join(conf.name()),
            report: base.join("statistics.csv"),
            source: base.join("src"),
        }
    }
}

pub fn run(
    tc: &Toolchain,
    program: &str,
    builds: &BuildList,
    jobs: usize,
    mode: RunMode,
) -> Result<()> {
    if mode != RunMode::DryRun {
        doctor::require(tc, builds)?;
    }

    let prog = Program::load(&tc.programs, program)?;
    for conf in builds.iter() {
        let dirs = Dirs::new(&tc.targets_root, &prog.name, conf);
        let vars = vars(tc, conf, &dirs, jobs);
        let mut commands = commands(&prog, &vars, &dirs)?;
        let report = dirs.report.clone();
        if conf.needs_ir_plugin() {
            commands.iter_mut().for_each(|c| {
                c.env("SS_REPORT_DEST", &report);
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
        let logs = dirs.build.join("btp-logs");
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
        if let Some(r) = IrReport::read(&report)? {
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

/// Estrapola i comandi da eseguire durante il processo di build e ci inserisce i parametri
/// di input.
fn commands(prog: &Program, vars: &Vars, dirs: &Dirs) -> Result<Vec<Command>> {
    let mut env = Vec::new();
    for (k, v) in &prog.build.env {
        let v = expand(v, vars).with_context(|| format!("{}: build.env.{k}", prog.name))?;
        env.push((k.clone(), v));
    }
    let mut commands = Vec::new();
    for (i, step) in prog.build.steps.iter().enumerate() {
        let ctx = || format!("{}: build.steps[{i}]", prog.name);
        let (program, args) = step
            .split_first()
            .with_context(|| format!("{}: empty command", ctx()))?;
        let mut cmd = Command::new(expand(program, vars).with_context(ctx)?);
        for a in args {
            cmd.arg(expand(a, vars).with_context(ctx)?);
        }
        cmd.current_dir(&dirs.build)
            .envs(env.iter().map(|(k, v)| (k, v)));
        commands.push(cmd);
    }
    Ok(commands)
}

fn expand(template: &str, vars: &Vars) -> Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let (c, after) = (rest.as_bytes()[i], &rest[i + 1..]);
        if after.starts_with(c as char) {
            // graffa raddoppiata: carattere letterale
            out.push(c as char);
            rest = &after[1..];
        } else if c == b'}' {
            bail!("unmatched '}}' in \"{template}\"");
        } else {
            let Some(end) = after.find('}') else {
                bail!("unclosed '{{' in \"{template}\"");
            };
            let name = &after[..end];
            match vars.get(name) {
                Some(v) => out.push_str(v),
                None => bail!(
                    "unknown placeholder {{{name}}} in \"{template}\" (known: {})",
                    vars.keys().copied().collect::<Vec<_>>().join(", ")
                ),
            }
            rest = &after[end + 1..];
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// Clona la directory da remoto, ne seleziona la versione specificata nel file toml e se c'è
/// bisogno (`copy_source == true`) la copia nella directory di build.
fn prepare(prog: &Program, dirs: &Dirs) -> Vec<Command> {
    let mut v = Vec::new();
    if !dirs.source.join(".git").is_dir() {
        let mut c = Command::new("git");
        c.args(["clone", "-q", &prog.source.git]).arg(&dirs.source);
        v.push(c);
    }

    let mut c = Command::new("git");
    c.arg("-C").arg(&dirs.source).args([
        "-c",
        "advice.detachedHead=false",
        "checkout",
        "-q",
        &prog.source.git_ref,
    ]);
    v.push(c);

    let mut c = Command::new("rm");
    c.arg("-rf").arg(&dirs.build).arg(&dirs.prefix);
    v.push(c);

    if prog.build.copy_source {
        let mut c = Command::new("git");
        c.args(["clone", "-q", "--shared", "--no-checkout"])
            .arg(&dirs.source)
            .arg(&dirs.build);
        v.push(c);

        let mut c = Command::new("git");
        c.arg("-C").arg(&dirs.build).args([
            "-c",
            "advice.detachedHead=false",
            "checkout",
            "-q",
            &prog.source.git_ref,
        ]);
        v.push(c);
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

fn run_quiet(c: &mut Command, mode: RunMode) -> Result<()> {
    if mode == RunMode::Verbose {
        println!("$ {c:?}");
    }
    let out = c.output().with_context(|| format!("cannot run {c:?}"))?;
    if !out.status.success() {
        bail!(
            "{c:?} failed ({}):\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn run_logged(c: &mut Command, log: &Path, mode: RunMode) -> Result<()> {
    if mode == RunMode::Verbose {
        println!("$ {c:?}");
    }
    let f = File::create(log).with_context(|| format!("cannot create {}", log.display()))?;
    c.stdout(f.try_clone()?).stderr(f);
    let status = c.status().with_context(|| format!("cannot run {c:?}"))?;
    if !status.success() {
        bail!(
            "{c:?} failed ({status}), log: {}\n{}",
            log.display(),
            tail(log, 20)
        );
    }
    Ok(())
}

fn tail(path: &Path, n: usize) -> String {
    let Ok(f) = File::open(path) else {
        return String::new();
    };
    let lines: Vec<String> = BufReader::new(f).lines().map_while(Result::ok).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn install(prog: &Program, dirs: &Dirs) -> Result<()> {
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

fn vars(tc: &Toolchain, conf: BuildConf, dirs: &Dirs, jobs: usize) -> Vars {
    Vars::from([
        ("cc", tc.clang.display().to_string()),
        ("cxx", tc.clangxx().display().to_string()),
        ("copt", tc.compile_flags(conf).join(" ")),
        ("ldopt", tc.link_flags(conf).join(" ")),
        ("jobs", jobs.to_string()),
        ("src", dirs.source.display().to_string()),
        ("build", dirs.build.display().to_string()),
        ("prefix", dirs.prefix.display().to_string()),
        ("report", dirs.report.display().to_string()),
    ])
}

#[derive(Clone)]
pub struct BuildListParser;

impl TypedValueParser for BuildListParser {
    type Value = BuildList;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        _arg: Option<&clap::Arg>,
        value: &OsStr,
    ) -> Result<BuildList, clap::Error> {
        let s = value
            .to_str()
            .ok_or_else(|| clap::Error::new(ErrorKind::InvalidUtf8).with_cmd(cmd))?;
        s.parse()
            .map_err(|e: Error| clap::Error::raw(ErrorKind::InvalidValue, format!("{e}\n")).with_cmd(cmd))
    }

    fn possible_values(&self) -> Option<Box<dyn Iterator<Item = PossibleValue> + '_>> {
        let confs = BuildConf::ALL.iter().filter_map(clap::ValueEnum::to_possible_value);
        let all = PossibleValue::new("all").help("every configuration");
        Some(Box::new(confs.chain(std::iter::once(all))))
    }
}