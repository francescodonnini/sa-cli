use crate::common::{clone, command, expand, repo_dir, run_quiet, spawn_logged, vars};
use crate::conf::{BuildConf, BuildList};
use crate::parsers::Outcome;
use crate::program::{Program, Suite};
use crate::toolchain::Toolchain;
use crate::{RunMode, doctor};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Instant;

pub fn run(
    tc: &Toolchain,
    program: &str,
    builds: &BuildList,
    jobs: usize,
    mode: RunMode,
) -> Result<()> {
    let mut aug = BuildList::single(BuildConf::Vanilla);
    builds.iter().for_each(|b| aug.push(b));
    if mode != RunMode::DryRun {
        doctor::require(tc, &aug)?;
    }
    let prog = Program::load(&tc.programs, program)?;
    let Some(test) = &prog.test else {
        bail!("{}: no [test] section in {}.toml", prog.name, prog.name);
    };
    for (name, repo) in &prog.repos {
        let dir = repo_dir(&tc.targets, &prog.name, name);
        for mut c in clone(&repo.git, &repo.git_ref, &dir) {
            if mode == RunMode::DryRun {
                println!("{c:?}");
            } else {
                run_quiet(&mut c, mode)?;
            }
        }
    }
    let out = if mode == RunMode::DryRun {
        tc.results.join("test").join(&prog.name).join("dry-run")
    } else {
        results_dir(&tc.results, &prog.name)?
    };
    let mut failed: BTreeMap<BuildConf, BTreeSet<String>> = BTreeMap::new();
    for build in aug.iter() {
        let dirs = tc.layout(&prog.name, build);
        if !dirs.prefix.is_dir() || !dirs.build.is_dir() {
            bail!("{build}: not built (run `btp build {} {build}`)", prog.name);
        }

        let mut vars = vars(tc, &prog, build, &dirs, jobs);
        let conf_out = out.join(build.name());
        if mode != RunMode::DryRun {
            fs::create_dir_all(&conf_out)?;
        }

        vars.insert("out".into(), conf_out.display().to_string());
        let mut names = BTreeSet::new();
        for suite in &test.suites {
            let ctx = || format!("{}: test.suite {}", prog.name, suite.name);
            let cwd = match &suite.cwd {
                Some(d) => PathBuf::from(expand(d, &vars).with_context(ctx)?),
                None => dirs.build.clone(),
            };
            let env = suite
                .env
                .iter()
                .map(|(k, v)| Ok((k.clone(), expand(v, &vars)?)))
                .collect::<Result<Vec<_>>>()
                .with_context(ctx)?;
            let mut cmd = command(&suite.cmd, &vars, &cwd, &env).with_context(ctx)?;
            if mode == RunMode::DryRun {
                println!("# {} {build} {}\n{cmd:?}", prog.name, suite.name);
                continue;
            }

            let log = conf_out.join(format!("{}.log", suite.name));
            let t0 = Instant::now();
            let status = spawn_logged(&mut cmd, &log, mode)?;
            let o = outcome(suite, status, &log)?;
            let secs = t0.elapsed().as_secs_f64();
            let counts = match o.counts {
                Some(c) if c.skipped > 0 => {
                    format!(", {}/{} passed, {} skipped", c.passed(), c.total, c.skipped)
                }
                Some(c) => format!(", {}/{} passed", c.passed(), c.total),
                None => String::new(),
            };
            if o.failed.is_empty() {
                println!("{build} {}: ok{counts} ({secs:.1} s)", suite.name);
            } else {
                println!(
                    "{build} {}: {} failed{counts} ({secs:.1} s), log: {}",
                    suite.name,
                    o.failed.len(),
                    log.display()
                );
            }
            names.extend(o.failed.into_iter().map(|n| format!("{}: {n}", suite.name)));
        }

        if mode != RunMode::DryRun {
            let list: String = names.iter().map(|n| format!("{n}\n")).collect();
            fs::write(out.join(format!("{build}.failed")), list)?;
        }
        failed.insert(build, names);
    }

    if mode != RunMode::DryRun {
        compare(&failed);
        println!("\nlogs in {}", out.display());
    }
    Ok(())
}

fn outcome(suite: &Suite, status: ExitStatus, log: &Path) -> Result<Outcome> {
    let text = fs::read(log).with_context(|| format!("cannot read {}", log.display()))?;
    let mut o = suite.failures.parse(&String::from_utf8_lossy(&text));
    if !status.success() && o.failed.is_empty() {
        o.failed.push(format!("({status})"));
    }
    Ok(o)
}

fn compare(failed: &BTreeMap<BuildConf, BTreeSet<String>>) {
    let Some(reference) = failed.get(&BuildConf::Vanilla) else {
        return;
    };
    println!("\n== failing only in the configuration (vs vanilla)");
    for (conf, names) in failed.iter().filter(|(c, _)| **c != BuildConf::Vanilla) {
        let extra: Vec<&String> = names.difference(reference).collect();
        if extra.is_empty() {
            println!("   {conf}: none");
            continue;
        }
        println!("   {conf}: {}", extra.len());
        for n in extra.iter().take(30) {
            println!("     {n}");
        }
    }
}

/// Crea una cartella in results/test/<programma>/<data-ora>/
fn results_dir(results: &Path, program: &str) -> Result<PathBuf> {
    let out = std::process::Command::new("date")
        .arg("+%Y%m%d-%H%M%S")
        .output()?;
    let stamp = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let dir = results.join("test").join(program).join(stamp);
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    Ok(dir)
}
