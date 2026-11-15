use crate::common::{NonEmptySet, expand_home};
use crate::parsers::{FailureParser, MetricParser};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer};
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
    pub bench: Option<Bench>,
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
    /// Limite soft di RLIMIT_NOFILE per i comandi di test.
    /// Se assente, i test ereditano il limite di btp.
    pub nofile: Option<u64>,
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bench {
    /// Misure per configurazione (valore di default dei casi).
    #[serde(default = "default_repeat")]
    pub repeat: u32,
    /// Esecuzioni scartate prima delle misure (valore di default dei casi).
    #[serde(default = "default_warmup")]
    pub warmup: u32,
    #[serde(rename = "case")]
    pub cases: Cases,
}

fn default_repeat() -> u32 {
    5
}

fn default_warmup() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "Vec<Case>")]
pub(crate) struct Cases(NonEmptySet<Case>);

impl TryFrom<Vec<Case>> for Cases {
    type Error = String;

    fn try_from(v: Vec<Case>) -> Result<Self, String> {
        let mut seen = BTreeSet::new();
        for c in &v {
            if !seen.insert(c.name()) {
                return Err(format!("duplicate bench case name \"{}\"", c.name()));
            }
        }
        NonEmptySet::try_from(v)
            .map(Cases)
            .map_err(|_| "no bench cases".to_string())
    }
}

impl Cases {
    pub fn list(&self) -> &NonEmptySet<Case> {
        &self.0
    }
}

/// Un caso di benchmark: una misura, ripetuta `repeat` volte per ogni configurazione.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Case {
    /// Un solo eseguibile, strumentato: la misura è il suo tempo o il suo output.
    Single(Single),
    /// Un server strumentato e un client non strumentato, che produce la misura.
    ClientServer(ClientServer),
}

/// I nomi sono unici (vedi `Cases`): due casi sono lo stesso caso se hanno lo stesso nome.
impl PartialEq for Case {
    fn eq(&self, other: &Self) -> bool {
        self.name() == other.name()
    }
}

impl Case {
    pub fn name(&self) -> &str {
        match self {
            Case::Single(c) => &c.name,
            Case::ClientServer(c) => &c.name,
        }
    }

    /// (warmup, repeat) del caso, con i valori di `[bench]` come default.
    pub fn runs(&self, bench: &Bench) -> (u32, u32) {
        let (w, r) = match self {
            Case::Single(c) => (c.warmup, c.repeat),
            Case::ClientServer(c) => (c.warmup, c.repeat),
        };
        (w.unwrap_or(bench.warmup), r.unwrap_or(bench.repeat))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Single {
    pub name: String,
    pub repeat: Option<u32>,
    pub warmup: Option<u32>,
    pub cmd: Vec<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub cpus: Option<String>,
    #[serde(default)]
    pub metric: MetricParser,
}

impl Single {
    pub fn process(&self) -> Process {
        Process {
            cmd: self.cmd.clone(),
            cwd: self.cwd.clone(),
            env: self.env.clone(),
            cpus: self.cpus.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientServer {
    pub name: String,
    pub repeat: Option<u32>,
    pub warmup: Option<u32>,
    pub server: Process,
    pub ready: Ready,
    /// Segnale per fermare il server (default TERM).
    #[serde(default)]
    pub stop: StopSignal,
    /// Comandi eseguiti a ogni misura, dopo `ready` e prima del client (es. `pgbench -i`).
    #[serde(default)]
    pub setup: Vec<Vec<String>>,
    pub client: Process,
    #[serde(deserialize_with = "output_metric")]
    pub metric: MetricParser,
}

/// Un processo di un benchmark: il comando di un caso single, il server o il client.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Process {
    pub cmd: Vec<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Lista di CPU per `taskset -c` (es. "0-5" o "1,3").
    pub cpus: Option<String>,
}

/// Quando il server è pronto: `{ tcp = "127.0.0.1:6380" }` oppure `{ delay = 2.0 }`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum Ready {
    Tcp(std::net::SocketAddr),
    Delay(f64),
}

#[derive(Debug, Default, Deserialize, Clone, Copy)]
#[serde(rename_all = "UPPERCASE")]
pub enum StopSignal {
    #[default]
    Term,
    Int,
    Quit,
}

impl StopSignal {
    pub fn number(self) -> i32 {
        match self {
            StopSignal::Term => libc::SIGTERM,
            StopSignal::Int => libc::SIGINT,
            StopSignal::Quit => libc::SIGQUIT,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            StopSignal::Term => "TERM",
            StopSignal::Int => "INT",
            StopSignal::Quit => "QUIT",
        }
    }
}

/// La metrica di un caso client-server viene dall'output del client: `time` misurerebbe la
/// durata del client, non il server. Errore al caricamento del toml.
fn output_metric<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<MetricParser, D::Error> {
    let m = MetricParser::deserialize(d)?;
    if !m.reads_output() {
        return Err(serde::de::Error::custom(
            "metric \"time\" is only for kind = \"single\": a client-server case needs a parser of the client output",
        ));
    }
    Ok(m)
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
        if program.test.as_ref().and_then(|t| t.nofile) == Some(0) {
            bail!("{}: test.nofile must be greater than 0", file.display());
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
