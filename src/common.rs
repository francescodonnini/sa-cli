use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use crate::conf::BuildConf;
use crate::program::{Program, Repo};
use crate::RunMode;
use crate::toolchain::Toolchain;

pub(crate) type Vars = BTreeMap<String, String>;

/// Contiene i percorsi usati durante il processo di build di un programma
/// * `build` è il posto in cui viene salvato il programma compilato,
/// * `prefix` è il percorso in cui viene salvata una configurazione,
/// * `report` è il percorso del file in cui il compilatore salva le metriche raccolte dai pass
///    del compilatore,
/// * `source` è il percorso in cui viene salvato il codice sorgente del programma.
pub(crate) struct Dirs {
    pub build: PathBuf,
    pub prefix: PathBuf,
    pub report: PathBuf,
    pub source: PathBuf,
}

impl Dirs {
    pub(crate) fn new(targets_root: &Path, program: &str, conf: BuildConf) -> Dirs {
        let base = targets_root.join(program);
        let build = base.join("build");
        Dirs {
            build: base.join(format!("build-{conf}")),
            prefix: base.join(conf.name()),
            report: build.join("statistics.csv"),
            source: base.join("src"),
        }
    }
}

/// Costruisce il percorso $SS_ROOT/<programma>/repos/<nome>
pub(crate) fn repo_dir(targets_root: &Path, program: &str, name: &str) -> PathBuf {
    targets_root.join(program).join("repos").join(name)
}

/// Costruisce un vettore di comandi per ottenere la versione corretta della repository di test o
/// benchmarks: Il vettore contiene i seguenti comandi:
/// + `git clone` della repository se non è già presente nel sistema ed effettua
/// + `git checkout` della versione specificata in `repo`
pub(crate) fn repo_commands(repo: &Repo, dir: &Path) -> Vec<Command> {
    let mut v = Vec::new();
    if !dir.join(".git").is_dir() {
        let mut c = Command::new("git");
        c.args(["clone", "-q", &repo.git]).arg(dir);
        v.push(c);
    }
    let mut c = Command::new("git");
    c.arg("-C").arg(dir)
        .args(["-c", "advice.detachedHead=false", "checkout", "-q", &repo.git_ref]);
    v.push(c);
    v
}

pub(crate) fn command(
    argv: &[String],
    vars: &Vars,
    cwd: &Path,
    env: &[(String, String)],
) -> Result<Command> {
    let (program, args) = argv.split_first().context("empty command")?;
    let mut command = Command::new(expand(program, vars)?);
    for a in args {
        command.arg(expand(a, vars)?);
    }
    command.current_dir(cwd).envs(env.iter().map(|(k, v)| (k, v)));
    Ok(command)
}

pub(crate) fn expand(template: &str, vars: &Vars) -> anyhow::Result<String> {
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
                    vars.keys().map(String::as_str).collect::<Vec<_>>().join(", ")
                ),
            }
            rest = &after[end + 1..];
        }
    }
    out.push_str(rest);
    Ok(out)
}

pub(crate) fn run_quiet(c: &mut Command, mode: RunMode) -> Result<()> {
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

pub(crate) fn run_logged(c: &mut Command, log: &Path, mode: RunMode) -> Result<()> {
    let status = spawn_logged(c, log, mode)?;
    if !status.success() {
        bail!(
            "{c:?} failed ({status}), log: {}\n{}",
            log.display(),
            tail(log, 20)
        );
    }
    Ok(())
}

pub(crate) fn spawn_logged(c: &mut Command, log: &Path, mode: RunMode) -> Result<ExitStatus> {
    if mode == RunMode::Verbose {
        println!("$ {c:?}");
    }
    let f = File::create(log).with_context(|| format!("cannot create {}", log.display()))?;
    c.stdin(Stdio::null()).stdout(f.try_clone()?).stderr(f);
    c.status().with_context(|| format!("cannot run {c:?}"))
}

fn tail(path: &Path, n: usize) -> String {
    let Ok(f) = File::open(path) else {
        return String::new();
    };
    let lines: Vec<String> = BufReader::new(f).lines().map_while(Result::ok).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

pub(crate) fn vars(tc: &Toolchain, program: &Program, conf: BuildConf, dirs: &Dirs, jobs: usize) -> Vars {
    let mut v = Vars::from([
        ("cc".into(), tc.clang.display().to_string()),
        ("cxx".into(), tc.clangxx().display().to_string()),
        ("copt".into(), tc.compile_flags(conf).join(" ")),
        ("ldopt".into(), tc.link_flags(conf).join(" ")),
        ("jobs".into(), jobs.to_string()),
        ("src".into(), dirs.source.display().to_string()),
        ("build".into(), dirs.build.display().to_string()),
        ("prefix".into(), dirs.prefix.display().to_string()),
        ("report".into(), dirs.report.display().to_string()),
    ]);
    for name in program.repos.keys() {
        let dir = repo_dir(&tc.targets_root, &program.name, name);
        v.insert(format!("repos.{name}"), dir.display().to_string());
    }
    v
}