use std::io::Write;
use crate::common::{NonEmptyVec, clone, command, expand, repo_dir, run_quiet, spawn_logged, vars};
use crate::conf::BuildConf;
use crate::parsers::Outcome;
use crate::program::{Program, Suite, Suites};
use crate::toolchain::Toolchain;
use crate::{RunMode, doctor};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Instant;

struct Tee(Option<File>);

impl Tee {
    fn line(&mut self, s: impl AsRef<str>) {
        let s = s.as_ref();
        println!("{s}");
        if let Some(f) = &mut self.0 {
            // un errore di scrittura non deve interrompere i test: la riga è comunque a schermo
            let _ = writeln!(f, "{s}");
        }
    }
}

pub fn run(
    tc: &Toolchain,
    program: &str,
    builds: &NonEmptyVec<BuildConf>,
    suites: Option<&NonEmptyVec<String>>,
    jobs: usize,
    mode: RunMode,
) -> Result<()> {
    let mut aug = NonEmptyVec::single(BuildConf::Vanilla);
    builds.into_iter().for_each(|b| {
        if !aug.contains(b) {
            aug.push(*b)
        }
    });
    if mode != RunMode::DryRun {
        doctor::require(tc, &aug)?;
    }
    let prog = Program::load(&tc.programs, program)?;
    let Some(test) = &prog.test else {
        bail!("{}: no [test] section in {}.toml", prog.name, prog.name);
    };
    let selected = select(&test.suites, suites)?;
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
    let mut tee = Tee(if mode == RunMode::DryRun {
        None
    } else {
        let path = out.join("output.txt");
        Some(File::create(&path).with_context(|| format!("cannot create {}", path.display()))?)
    });
    let mut summary = String::from("conf\tsuite\tstatus\tpassed\ttotal\tskipped\tfailed\tseconds\n");
    let mut failed: BTreeMap<BuildConf, BTreeSet<String>> = BTreeMap::new();
    for &build in aug.iter() {
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
        for suite in &selected {
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
            let exit = spawn_logged(&mut cmd, &log, mode)?;
            let o = outcome(suite, exit, &log)?;
            let secs = t0.elapsed().as_secs_f64();
            let result = if o.failed.is_empty() { "ok" } else { "failed" };
            let counts = match o.counts {
                Some(c) if c.skipped > 0 => {
                    format!(", {}/{} passed, {} skipped", c.passed(), c.total, c.skipped)
                }
                Some(c) => format!(", {}/{} passed", c.passed(), c.total),
                None => String::new(),
            };
            let (passed, total, skipped) = match o.counts {
                Some(c) => (c.passed().to_string(), c.total.to_string(), c.skipped.to_string()),
                None => Default::default(),
            };
            if o.failed.is_empty() {
                tee.line(format!("{build} {}: ok{counts} ({secs:.1} s)", suite.name));
            } else {
                tee.line(format!(
                    "{build} {}: {} failed{counts} ({secs:.1} s), log: {}",
                    suite.name,
                    o.failed.len(),
                    log.display()
                ));
            }
            summary.push_str(&format!(
                "{build}\t{}\t{result}\t{passed}\t{total}\t{skipped}\t{}\t{secs:.1}\n",
                suite.name,
                o.failed.len()
            ));
            names.extend(o.failed.into_iter().map(|n| format!("{}: {n}", suite.name)));
        }

        // fine della configurazione: l'elenco dei suoi test falliti
        if mode != RunMode::DryRun {
            let list: String = names.iter().map(|n| format!("{n}\n")).collect();
            let path = out.join(format!("{build}.failed"));
            fs::write(&path, list).with_context(|| format!("cannot write {}", path.display()))?;
        }
        failed.insert(build, names);
    }

    // fine dell'esecuzione: riepilogo tabellare, confronto con vanilla, cartella dei log
    if mode != RunMode::DryRun {
        let path = out.join("summary.tsv");
        fs::write(&path, &summary).with_context(|| format!("cannot write {}", path.display()))?;
        compare(&failed, &mut tee);
        tee.line(format!("\nlogs in {}", out.display()));
    }
    Ok(())
}

fn select<'a>(
    all: &'a Suites,
    names: Option<&NonEmptyVec<String>>,
) -> Result<NonEmptyVec<&'a Suite>> {
    let all = all.list();
    let Some(names) = names else {
        return NonEmptyVec::try_from(all.iter().collect::<Vec<_>>());
    };
    let unknown: Vec<&str> = names
        .iter()
        .filter(|n| !all.iter().any(|s| &s.name == *n))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        let known: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        bail!(
            "unknown suite: {} (available: {})",
            unknown.join(", "),
            known.join(", ")
        );
    }
    NonEmptyVec::try_from(all.iter().filter(|s| names.contains(&s.name)).collect::<Vec<_>>())
}

fn outcome(suite: &Suite, status: ExitStatus, log: &Path) -> Result<Outcome> {
    let text = fs::read(log).with_context(|| format!("cannot read {}", log.display()))?;
    let mut o = suite.failures.parse(&String::from_utf8_lossy(&text));
    if !status.success() && o.failed.is_empty() {
        o.failed.push(format!("({status})"));
    }
    Ok(o)
}

fn compare(failed: &BTreeMap<BuildConf, BTreeSet<String>>, tee: &mut Tee) {
    let Some(reference) = failed.get(&BuildConf::Vanilla) else {
        return;
    };
    tee.line("\n== failing only in the configuration (vs vanilla)");
    for (conf, names) in failed.iter().filter(|(c, _)| **c != BuildConf::Vanilla) {
        let extra: Vec<&String> = names.difference(reference).collect();
        if extra.is_empty() {
            tee.line(format!("   {conf}: none"));
            continue;
        }
        tee.line(format!("   {conf}: {}", extra.len()));
        for n in extra.iter().take(30) {
            tee.line(format!("     {n}"));
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
