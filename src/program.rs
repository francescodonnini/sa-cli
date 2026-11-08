use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    #[serde(skip)]
    pub name: String,
    pub source: Source,
    pub build: Build,
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
