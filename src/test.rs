use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Instant;
use crate::conf::{BuildConf, BuildList};
use crate::toolchain::Toolchain;
use anyhow::{bail, Result, Context};
use crate::{doctor, RunMode};
use crate::common::{vars, Dirs, expand, command, spawn_logged, repo_dir, repo_commands, run_quiet};
use crate::program::{Program, Suite};

const PARSERS: &[&str] = &["exit-code"];

fn failures(suite: &Suite, status: ExitStatus, log: &Path) -> Result<Vec<String>> {
    let text = String::from_utf8_lossy(&fs::read(log)?).into_owned();
    let mut f = match suite.failures.as_str() {
        "exit-code" => Vec::new(),
        "prove" => prove(&text),
        other => bail!("{}: unknown failure parser '{other}'", suite.name),
    };
    if !status.success() && f.is_empty() {
        f.push(format!("({status})"));
    }
    Ok(f)
}

fn prove(log: &str) -> Vec<String> {
    let mut v: Vec<String> = log
        .lines()
        .filter_map(|l| {
            let (name, rest) = l.split_once(char::is_whitespace)?;
            let failed = name.ends_with(".t") && rest.trim_start().starts_with("(Wstat");
            failed.then(|| name.trim_start_matches("./").to_string())
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

pub fn run(tc: &Toolchain, program: &str, builds: &BuildList, jobs: usize, mode: RunMode) -> Result<()> {
    let mut aug = BuildList::single(BuildConf::Vanilla);
    builds.iter().for_each(|b| aug.push(b));
    if mode != RunMode::DryRun {
        doctor::require(tc, &aug)?;
    }

    let prog = Program::load(&tc.programs, program)?;
    for (name, repo) in &prog.repos {
        let dir = repo_dir(&tc.targets_root, &prog.name, name);
        for mut c in repo_commands(repo, &dir) {
            if mode == RunMode::DryRun {
                println!("{c:?}");
            } else {
                run_quiet(&mut c, mode)?;
            }
        }
    }
    let Some(test) = &prog.test else {
        bail!("{}: no [test] section in {}.toml", prog.name, prog.name);
    };
    let out = if mode == RunMode::DryRun {
        tc.results.join("test").join(&prog.name).join("dry-run")
    } else {
        results_dir(&tc.results, &prog.name)?
    };
    let mut failed: BTreeMap<BuildConf, BTreeSet<String>> = BTreeMap::new();
    for build in aug.iter() {
        let dirs = Dirs::new(&tc.targets_root, &prog.name, build);
        if !dirs.prefix.is_dir() || !dirs.build.is_dir() {
            bail!("{build}: not built (run `btp build {} {build}`)", prog.name);
        }

        let mut vars = vars(tc, &prog, build, &dirs, jobs);
        let conf_out = out.join(build.name());
        if mode != RunMode::DryRun {
            fs::create_dir_all(&conf_out)?;
        }

        vars.insert("out".into(), out.display().to_string());
        let mut names = BTreeSet::new();
        for suite in &test.suites {
            if !PARSERS.contains(&suite.failures.as_str()) {
                bail!("{}: test.suite {}: unknown failure parser '{}' (known: {})",
              prog.name, suite.name, suite.failures, PARSERS.join(", "));
            }
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
            let mut cmd = command(&suite.commands, &vars, &cwd, &env).with_context(ctx)?;
            if mode == RunMode::DryRun {
                println!("# {} {build} {}\n{cmd:?}", prog.name, suite.name);
                continue;
            }

            let log = out.join(format!("{build}-{}.log", suite.name));
            let t0 = Instant::now();
            let status = spawn_logged(&mut cmd, &log, mode)?;
            let f = failures(suite, status, &log)?;
            let secs = t0.elapsed().as_secs_f64();
            if f.is_empty() {
                println!("{build} {}: ok ({secs:.1} s)", suite.name);
            } else {
                println!("{build} {}: {} failed ({secs:.1} s), log: {}", suite.name, f.len(), log.display());
            }
            names.extend(f.into_iter().map(|n| format!("{}: {n}", suite.name)));
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

fn compare(failed: &BTreeMap<BuildConf, BTreeSet<String>>) {
    let Some(reference) = failed.get(&BuildConf::Vanilla) else { return };
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
    let out = std::process::Command::new("date").arg("+%Y%m%d-%H%M%S").output()?;
    let stamp = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let dir = results.join("test").join(program).join(stamp);
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    Ok(dir)
}