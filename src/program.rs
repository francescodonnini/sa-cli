use crate::common;
use crate::common::Vars;
use crate::parsers::FailureParser;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub struct Template(String);

impl Template {
    pub(crate) fn expand(&self, vars: &Vars) -> Result<String> {
        common::expand(&self.0, vars)
    }
}

pub(crate) type Argv = Vec<String>;

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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub git: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
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
    pub suites: Vec<Suite>,
}

#[derive(Debug, Deserialize)]
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
        let path = dir.join(format!("{name}.toml"));
        let text =
            fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let mut p: Program =
            toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
        p.name = name.to_string();
        Ok(p)
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
