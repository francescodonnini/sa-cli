use crate::RunMode;
use crate::conf::BuildConf;
use crate::program::Program;
pub(crate) use crate::toolchain::{Layout, Toolchain};
use anyhow::{Context, Error, Result, anyhow, bail};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::str::FromStr;

pub(crate) type Vars = BTreeMap<String, String>;

#[derive(Clone, Debug)]
pub struct NonEmptyVec<T> {
    head: T,
    tail: Vec<T>,
}

impl<T: PartialEq> TryFrom<Vec<T>> for NonEmptyVec<T> {
    type Error = Error;

    fn try_from(items: Vec<T>) -> std::result::Result<Self, Self::Error> {
        let mut it = items.into_iter();
        let head = it.next().ok_or_else(|| anyhow!("empty list"))?;
        let mut l = Self::single(head);
        it.for_each(|x| l.push(x));
        Ok(l)
    }
}

impl<T> NonEmptyVec<T> {
    pub fn single(one: T) -> Self {
        Self {
            head: one,
            tail: vec![],
        }
    }

    pub fn first(&self) -> &T {
        &self.head
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        std::iter::once(&self.head).chain(self.tail.iter())
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> + '_ {
        std::iter::once(&mut self.head).chain(self.tail.iter_mut())
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn len(&self) -> usize {
        1 + self.tail.len()
    }
}

impl<T: PartialEq> NonEmptyVec<T> {
    pub fn contains(&self, elem: &T) -> bool {
        &self.head == elem || self.tail.contains(elem)
    }

    pub fn push(&mut self, elem: T) {
        self.tail.push(elem);
    }
}

impl<'a, T> IntoIterator for &'a NonEmptyVec<T> {
    type Item = &'a T;
    type IntoIter = std::iter::Chain<std::iter::Once<&'a T>, std::slice::Iter<'a, T>>;

    fn into_iter(self) -> Self::IntoIter {
        std::iter::once(&self.head).chain(self.tail.iter())
    }
}

impl FromStr for NonEmptyVec<String> {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        let names = s
            .split(',')
            .map(|p| match p.trim() {
                "" => Err(format!("empty name in \"{s}\"")),
                p => Ok(p.to_string()),
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        // split restituisce sempre almeno un pezzo, quindi names non è mai vuoto
        NonEmptyVec::try_from(names).map_err(|e| e.to_string())
    }
}

/// Cartella di una repository ausiliaria: <targets>/<programma>/repos/<nome>.
pub(crate) fn repo_dir(targets: &Path, program: &str, name: &str) -> PathBuf {
    targets.join(program).join("repos").join(name)
}

/// Costruisce un vettore di comandi per ottenere la versione corretta della repository di test o
/// benchmarks: Il vettore contiene i seguenti comandi:
/// + `git clone` della repository se non è già presente nel sistema ed effettua
/// + `git checkout` della versione specificata in `repo`
pub(crate) fn clone(git: &str, git_ref: &str, dir: &Path) -> Vec<Command> {
    let mut v = Vec::new();
    if !dir.join(".git").is_dir() {
        let mut c = Command::new("git");
        c.args(["clone", "-q", git]).arg(dir);
        v.push(c);
    }
    v.push(checkout_only(dir, git_ref));
    v
}

pub(crate) fn checkout_only(dir: &Path, git_ref: &str) -> Command {
    let mut c = Command::new("git");
    c.arg("-C")
        .arg(dir)
        .args(["-c", "advice.detachedHead=false", "checkout", "-q", git_ref]);
    c
}

/// Crea un comando pronto per essere eseguito. `argv` contiene il nome del programma da eseguire
/// come primo elemento
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
    command
        .current_dir(cwd)
        .envs(env.iter().map(|(k, v)| (k, v)));
    Ok(command)
}

/// Sostituisce tutti i simboli che appaiono in `template` avvolti da due parentesi graffe `{` `}`
/// con il valore associato al simbolo in `vars`. Se in `template` comparisse un simbolo non
/// presente in `vars` la funzione termina in un errore.
pub(crate) fn expand(template: &str, vars: &Vars) -> Result<String> {
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
                    vars.keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
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

/// Costruisce una tabella di coppie chiave-valore dove le chiavi potrebbero apparire nella lista
/// dei comandi di build, e i valori sono presi da `Toolchain` e `Dirs`.
pub(crate) fn vars(
    tc: &Toolchain,
    program: &Program,
    conf: BuildConf,
    dirs: &Layout,
    jobs: usize,
) -> Vars {
    let mut v = Vars::from([
        ("cc".into(), tc.clang.display().to_string()),
        ("copt".into(), tc.compile_flags(conf).join(" ")),
        ("ldopt".into(), tc.link_flags(conf).join(" ")),
        ("jobs".into(), jobs.to_string()),
        ("src".into(), dirs.source.display().to_string()),
        ("build".into(), dirs.build.display().to_string()),
        ("prefix".into(), dirs.prefix.display().to_string()),
        ("report".into(), dirs.report.display().to_string()),
    ]);
    if let Some(cxx) = &tc.clangxx {
        v.insert("cxx".into(), cxx.display().to_string());
    }
    for name in program.repos.keys() {
        let dir = repo_dir(&tc.targets, &program.name, name);
        v.insert(format!("repos.{name}"), dir.display().to_string());
    }
    v
}

pub(crate) fn expand_home(path: &Path) -> Result<PathBuf> {
    match path.strip_prefix("~") {
        Ok(rest) => {
            let home = std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .context("HOME is not set")?;
            Ok(PathBuf::from(home).join(rest))
        }
        Err(_) => Ok(PathBuf::from(path)),
    }
}