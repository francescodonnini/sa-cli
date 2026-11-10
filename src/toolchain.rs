use crate::conf::BuildConf;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::{env, fs, io};

const TEMPLATE: &str = include_str!("toolchain.toml");

/// Crea un file .toml in `path` solamente se già non esiste un file in quel percorso,
/// almeno che `force` sia `true`; in tal caso il file viene sovrascritto.
///
/// Fallisce se non è possibile creare una o più directory presenti in `path` oppure se non
/// è possibile creare o scrivere il file in `path`.
pub(crate) fn init(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .with_context(|| format!("cannot create directory {}", dir.display()))?;
    }
    fs::write(path, TEMPLATE).with_context(|| format!("cannot write {}", path.display()))?;
    eprintln!("created: {}", path.display());
    Ok(())
}

/// Contiene percorsi a cartelle e programmi utili durante il processo di build, testing e
/// benchmarking.
pub struct Toolchain {
    /// `clang` è il percorso all'eseguibile del compilatore.
    pub clang: PathBuf,
    /// `clangxx` è il percorso al compilatore C++; se assente viene ricavato da `clang`
    /// (`clang-20` -> `clang++-20`),
    pub clangxx: Option<PathBuf>,
    /// `fun_pass` è il percorso al `FunctionPass` (LLVM IR) che applica la maschera, per esempio
    /// `~(1<<47)` se si usa la paginazione a quattro livelli, agli indirizzi di destinazione delle
    /// scritture.
    pub fun_pass: PathBuf,
    /// `programs` è il percorso alla cartella contenente i file .toml che specificano come effettuare
    /// la build, test e benchmarking.
    pub programs: PathBuf,
    /// `results` è il percorso alla cartella dove vengono salvati i risultati delle esecuzioni.
    pub results: PathBuf,
    /// `runtime` è il percorso all'oggetto condiviso caricato nello spazio d'indirizzamento del
    /// programma protetto.
    pub runtime: PathBuf,
    pub targets: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolchainFile {
    pub clang: PathBuf,
    pub clangxx: Option<PathBuf>,
    pub fun_pass: Option<PathBuf>,
    pub programs: Option<PathBuf>,
    pub results: Option<PathBuf>,
    pub runtime: Option<PathBuf>,
    pub targets: Option<PathBuf>,
}

/// Dove vive su disco una coppia (programma, configurazione), sotto `targets`:
/// + `build` è il posto in cui viene salvato il programma compilato,
/// + `prefix` è il percorso in cui viene salvata una configurazione,
/// + `report` è il percorso del file in cui il compilatore salva le metriche raccolte dai pass
///    del compilatore,
/// + `source` è il percorso in cui viene salvato il codice sorgente del programma.
/// ```text
/// <targets>/<programma>/src          sorgente (condiviso tra le configurazioni)
/// <targets>/<programma>/build-<conf> cartella di build, log e statistics.csv
/// <targets>/<programma>/<conf>       installazione (prefix)
/// ```
pub(crate) struct Layout {
    pub build: PathBuf,
    pub prefix: PathBuf,
    pub report: PathBuf,
    pub source: PathBuf,
}

impl Layout {
    /// E.g.
    /// targets: ~/targets
    /// program: postgres
    /// conf:    StoreProtection ("mask")
    /// build:   ~/targets/postgres/build-mask
    /// prefix:  ~/targets/postgres/mask
    /// report:  ~/targets/postgres/build-mask/statistics.csv
    /// source:  ~/targets/postgres/src
    fn new(targets: &Path, program: &str, conf: BuildConf) -> Layout {
        let base = targets.join(program);
        let build = base.join(format!("build-{conf}"));
        Layout {
            report: build.join("statistics.csv"),
            build,
            prefix: base.join(conf.name()),
            source: base.join("src"),
        }
    }
}

impl Toolchain {
    pub(crate) fn layout(&self, program: &str, conf: BuildConf) -> Layout {
        Layout::new(&self.targets, program, conf)
    }

    /// Legge il file .toml in `path`. Per quanto riguarda `fun_pass`, `programs`, `results`,
    /// `runtime` e `targets` se non sono presenti valori nel file .toml indicato, allora vengono
    /// assegnati a valori di default.
    /// `fun_pass` <- ~/shadow_stack/build/ShadowStack.so
    /// `programs` <- {project root}/programs
    /// `results` <- ~/results
    /// `runtime` <- ~/runtime
    /// `targets` <- ~/targets
    /// Per quanto riguarda `clang` il valore deve essere obbligatoriamente specificato nel file
    /// .toml.
    ///
    /// La funzione fallisce nel caso in cui non è possibile leggere il contenuto del file indicato
    /// in `path` oppure se presenta degli errori di formato.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                bail!("{} does not exist (run `btp init`)", path.display())
            }
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        let file: ToolchainFile =
            toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
        let home = env::var_os("HOME")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        let r = Resolver {
            home: home.as_deref(),
            base: path.parent().unwrap_or(Path::new(".")),
        };
        let clang = r.exe(&file.clang).context("clang")?;
        let clangxx = match file.clangxx {
            Some(p) => Some(r.exe(&p).context("clangxx")?),
            None => Self::from_clang(&clang),
        };
        Ok(Self {
            clang,
            clangxx,
            fun_pass: r
                .or_home(file.fun_pass, "shadow_stack/build/ShadowStack.so")
                .context("fun_pass")?,
            programs: match file.programs {
                Some(p) => r.path(&p).context("programs")?,
                None => PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/programs")),
            },
            results: r.or_home(file.results, "results").context("results")?,
            runtime: r
                .or_home(file.runtime, "runtime/build")
                .context("runtime")?,
            targets: r.or_home(file.targets, "targets").context("targets")?,
        })
    }

    /// Il compilatore C++ ricavato da `clang`, solo se esiste.
    fn from_clang(clang: &Path) -> Option<PathBuf> {
        let name = clang.file_name().unwrap_or_default().to_string_lossy();
        let cxx = match name.strip_prefix("clang") {
            Some(rest) => format!("clang++{rest}"),
            None => format!("{name}++"),
        };
        let cxx = clang.with_file_name(cxx);
        let bare = cxx.parent().is_some_and(|d| d.as_os_str().is_empty());
        let found = if bare {
            env::var_os("PATH")
                .is_some_and(|paths| env::split_paths(&paths).any(|d| d.join(&cxx).is_file()))
        } else {
            cxx.is_file()
        };
        found.then_some(cxx)
    }

    pub(crate) fn compile_flags(&self, conf: BuildConf) -> Vec<String> {
        let mut v = Vec::new();
        if conf.needs_ir_plugin() {
            v.push(format!("-fpass-plugin={}", self.fun_pass.display()));
        }
        for opt in conf.backend_options() {
            v.push("-mllvm".into());
            v.push(opt.into());
        }
        v
    }

    pub(crate) fn link_flags(&self, conf: BuildConf) -> Vec<String> {
        if !conf.uses_runtime() {
            return Vec::new();
        }
        let rt = self.runtime.display();
        vec![
            format!("{rt}/stckcpy.o"),
            "-Wl,-e,relocate_stack".into(),
            format!("-L{rt}"),
            format!("-Wl,-rpath,{rt}"),
            "-Wl,--push-state,--no-as-needed".into(),
            "-lruntime".into(),
            "-Wl,--pop-state".into(),
        ]
    }
}

struct Resolver<'a> {
    home: Option<&'a Path>,
    base: &'a Path,
}

impl Resolver<'_> {
    fn home(&self) -> Result<&Path> {
        self.home
            .context("HOME is not set: cannot expand `~` or use a default path")
    }

    /// "~/x" -> $HOME/x; relativo -> base/x; assoluto invariato.
    fn path(&self, p: &Path) -> Result<PathBuf> {
        match p.strip_prefix("~") {
            Ok(rest) => Ok(self.home()?.join(rest)),
            Err(_) => Ok(self.base.join(p)),
        }
    }

    /// Come `path`, ma un nome semplice ("clang") resta invariato: lo cerca `Command` nel PATH.
    fn exe(&self, p: &Path) -> Result<PathBuf> {
        let bare = p.parent().is_some_and(|d| d.as_os_str().is_empty()) && p != Path::new("~");
        if bare {
            Ok(p.to_path_buf())
        } else {
            self.path(p)
        }
    }

    /// Il valore del file, se presente; altrimenti $HOME/`default`.
    fn or_home(&self, from_file: Option<PathBuf>, default: &str) -> Result<PathBuf> {
        match from_file {
            Some(p) => self.path(&p),
            None => Ok(self.home()?.join(default)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_valid() {
        toml::from_str::<ToolchainFile>(TEMPLATE).unwrap();
    }
}
