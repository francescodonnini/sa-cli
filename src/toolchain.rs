use crate::conf::BuildConf;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::{env, fs};

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
/// + `clang` è il percorso all'eseguibile del compilatore,
/// + `fun_pass` è il percorso al `FunctionPass` (LLVM IR) che applica la maschera, per esempio
///   `~(1<<47)` se si usa la paginazione a quattro livelli, agli indirizzi di destinazione delle
///   scritture,
/// + `programs` è il percorso alla cartella contenente i file .toml che specificano come effettuare
///   la build, test e benchmarking,
/// + `results` è il percorso alla cartella dove vengono salvati i risultati delle esecuzioni,
/// + `runtime` è il percorso all'oggetto condiviso caricato nello spazio d'indirizzamento del
///   programma protetto.
pub struct Toolchain {
    pub clang: PathBuf,
    pub fun_pass: PathBuf,
    pub programs: PathBuf,
    pub results: PathBuf,
    pub runtime: PathBuf,
    pub targets: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolchainFile {
    pub clang: PathBuf,
    pub fun_pass: Option<PathBuf>,
    pub programs: Option<PathBuf>,
    pub results: Option<PathBuf>,
    pub runtime: Option<PathBuf>,
    pub targets: Option<PathBuf>,
}

impl Toolchain {
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
        if !path.exists() {
            bail!("{} does not exist (run `btp init`)", path.display());
        }
        let text =
            fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let file: ToolchainFile =
            toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
        let home = PathBuf::from(env::var_os("HOME").unwrap_or_default());
        let base = path.parent().unwrap_or(Path::new("."));
        let pick = |from_file: Option<PathBuf>, default: PathBuf| {
            from_file.map_or(default, |p| Self::resolve(&p, &home, base))
        };
        Ok(Self {
            clang: Self::resolve(&file.clang, &home, base),
            fun_pass: pick(
                file.fun_pass,
                home.join("shadow_stack/build/ShadowStack.so"),
            ),
            programs: pick(
                file.programs,
                PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/programs")),
            ),
            results: pick(file.results, home.join("results")),
            runtime: pick(file.runtime, home.join("runtime/build")),
            targets: pick(file.targets, home.join("targets")),
        })
    }

    /// "~/x" diventa $HOME/x
    /// Un percorso relativo viene concatenato con `base`
    /// Un percorso assoluto viene ritornato così com'è
    fn resolve(p: &Path, home: &Path, base: &Path) -> PathBuf {
        match p.strip_prefix("~") {
            Ok(rest) => home.join(rest),
            Err(_) => base.join(p),
        }
    }

    pub(crate) fn clangxx(&self) -> PathBuf {
        let mut name = self.clang.file_name().unwrap_or_default().to_os_string();
        name.push("++");
        self.clang.with_file_name(name)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_valid() {
        toml::from_str::<ToolchainFile>(TEMPLATE).unwrap();
    }
}
