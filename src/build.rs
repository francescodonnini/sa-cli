use crate::conf::{BuildConf, BuildList};
use crate::expand::{Vars, expand};
use crate::ir_report::IrReport;
use crate::program::Program;
use crate::toolchain::Toolchain;
use crate::{doctor, verify, RunMode};
use anyhow::{Context, Result, bail};
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Dirs {
    pub src: PathBuf,
    pub build: PathBuf,
    pub prefix: PathBuf,
    pub report: PathBuf,
}

impl Dirs {
    pub fn new(targets_root: &Path, program: &str, conf: BuildConf) -> Dirs {
        let base = targets_root.join(program);
        let build = base.join(format!("build-{conf}"));
        Dirs {
            src: base.join("src"),
            build,
            prefix: base.join(conf.name()),
            report: base.join(conf.name()),
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
        let report = dirs.build.join("stat-report.csv");
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

fn prepare(prog: &Program, dirs: &Dirs) -> Vec<Command> {
    let mut v = Vec::new();
    if !dirs.src.join(".git").is_dir() {
        let mut c = Command::new("git");
        c.args(["clone", "-q", &prog.source.git]).arg(&dirs.src);
        v.push(c);
    }

    let mut c = Command::new("git");
    c.arg("-C").arg(&dirs.src).args([
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
            .arg(&dirs.src)
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

pub fn vars(tc: &Toolchain, conf: BuildConf, dirs: &Dirs, jobs: usize) -> Vars {
    Vars::from([
        ("cc", tc.clang.display().to_string()),
        ("cxx", tc.clangxx().display().to_string()),
        ("copt", tc.compile_flags(conf).join(" ")),
        ("ldopt", tc.link_flags(conf).join(" ")),
        ("jobs", jobs.to_string()),
        ("src", dirs.src.display().to_string()),
        ("build", dirs.build.display().to_string()),
        ("prefix", dirs.prefix.display().to_string()),
        ("report", dirs.report.display().to_string()),
    ])
}

pub fn commands(prog: &Program, vars: &Vars, dirs: &Dirs) -> Result<Vec<Command>> {
    let mut env = Vec::new();
    for (k, v) in &prog.build.env {
        let v = expand(v, vars).with_context(|| format!("{}: build.env.{k}", prog.name))?;
        env.push((k.clone(), v));
    }
    let mut cmds = Vec::new();
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
        cmds.push(cmd);
    }
    Ok(cmds)
}
