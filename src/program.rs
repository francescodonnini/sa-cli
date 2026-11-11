use crate::common::{NonEmptySet, expand_home};
use crate::parsers::FailureParser;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(try_from = "Vec<Suite>")]
pub(crate) struct Suites(NonEmptySet<Suite>);

impl TryFrom<Vec<Suite>> for Suites {
    type Error = String;

    fn try_from(v: Vec<Suite>) -> Result<Self, String> {
        let mut seen = BTreeSet::new();
        for s in &v {
            if !seen.insert(s.name.as_str()) {
                return Err(format!("duplicate test suite name \"{}\"", s.name));
            }
        }
        NonEmptySet::try_from(v)
            .map(Suites)
            .map_err(|_| "no test suites".to_string())
    }
}

impl Suites {
    pub fn list(&self) -> &NonEmptySet<Suite> {
        &self.0
    }
}

/// Contiene tutte le informazioni necessarie per il processo di build, testing e profiling di un
/// certo programma.
/// + `source` contiene la posizione della repository git e la versione (commit),
/// + `build` contiene i passi necessari per compilare il programma (comandi bash),
/// + `test` contiene i casi di test da eseguire (?),
/// + `repos` contiene altri sorgenti che vengono usati per eseguire test o benchmark,
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    #[serde(skip)]
    pub name: String,
    pub source: Source,     // tutte le fasi
    pub build: Build,       // build
    pub test: Option<Test>, // test
    #[serde(default)]
    pub repos: BTreeMap<String, Repo>, // test & benchmarks
    // placeholder
    pub bench: Option<toml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Source {
    Git {
        git: String,
        #[serde(rename = "ref")]
        git_ref: String,
    },
    Path {
        path: PathBuf,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    /// Esporta CC, CXX, CFLAGS, CXXFLAGS e LDFLAGS (per autotools, CMake, Make)
    #[serde(default)]
    pub export_flags: bool,
    #[serde(default)]
    pub copy_source: bool,
    pub steps: Vec<Vec<String>>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub install: BTreeMap<PathBuf, PathBuf>,
    #[serde(default)]
    pub verify: Vec<Verify>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Test {
    #[serde(rename = "suite")]
    pub suites: Suites,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    pub name: String,
    pub cmd: Vec<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub failures: FailureParser,
}

/// Contiene la posizione di una sorgente (git), la versione selezionata (commit) e la lista di
/// comandi necessari per compilare la sorgente.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repo {
    pub git: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// Passi di build (compilatore di sistema, senza strumentazione); li userà bench
    #[serde(default)]
    pub build: Vec<Vec<String>>,
}

impl Program {
    pub fn load(dir: &Path, name: &str) -> Result<Program> {
        let file = dir.join(format!("{name}.toml"));
        let text =
            fs::read_to_string(&file).with_context(|| format!("cannot read {}", file.display()))?;
        let mut program: Program =
            toml::from_str(&text).with_context(|| format!("invalid {}", file.display()))?;
        program.name = name.to_string();
        if let Source::Path { path } = &mut program.source {
            *path =
                expand_home(path).with_context(|| format!("{}: source.path", path.display()))?;
            if !path.is_absolute() {
                bail!(
                    "{}: source.path must be absolute or start with ~",
                    file.display()
                );
            }
            if program.build.copy_source {
                bail!("{}: copy_source requires a git source", file.display());
            }
        }
        Ok(program)
    }

    /// Vero se un passo o una variabile d'ambiente della build usa `{name}`.
    pub fn build_uses(&self, name: &str) -> bool {
        let p = format!("{{{name}}}");
        self.build.steps.iter().flatten().any(|a| a.contains(&p))
            || self.build.env.values().any(|v| v.contains(&p))
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum Verify {
    Path(String),
    Detailed {
        path: String,
        #[serde(default = "default_min_refs")]
        min_refs: usize,
    },
}

fn default_min_refs() -> usize {
    100
}

impl Verify {
    pub fn path(&self) -> &str {
        match self {
            Verify::Path(p) | Verify::Detailed { path: p, .. } => p,
        }
    }

    pub fn min_refs(&self) -> usize {
        match self {
            Verify::Path(_) => default_min_refs(),
            Verify::Detailed { min_refs, .. } => *min_refs,
        }
    }
}
