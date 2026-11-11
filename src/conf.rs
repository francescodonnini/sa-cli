use crate::common::NonEmptySet;
use anyhow::{Context, Error, anyhow, bail};
use clap::builder::PossibleValue;
use std::cmp::PartialEq;
use std::fmt;
use std::str::FromStr;

/// Build configurations list
///     Vanilla: program is compiled as is
///     RuntimeOnly: relocate_stack + libruntime, no custom pass
///     StoreProtection: runtime + store masking
///     FunctionProtection: runtime + function protection
///     Full: runtime + store masking + function protection
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BuildConf {
    Vanilla,
    RuntimeOnly,
    StoreProtection,
    FunctionProtection,
    Full,
}

impl BuildConf {
    pub const ALL: [BuildConf; 5] = [
        BuildConf::Vanilla,
        BuildConf::RuntimeOnly,
        BuildConf::StoreProtection,
        BuildConf::FunctionProtection,
        BuildConf::Full,
    ];

    pub fn backend_options(self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.protects_stores() {
            v.push("-x86-shadow-stack-btr");
        }
        if self.protects_functions() {
            v.push("-x86-shadow-stack-pass");
        }
        v
    }

    pub const fn name(&self) -> &'static str {
        match self {
            BuildConf::Vanilla => "vanilla",
            BuildConf::RuntimeOnly => "ro",
            BuildConf::StoreProtection => "mask",
            BuildConf::FunctionProtection => "func",
            BuildConf::Full => "full",
        }
    }

    pub fn needs_ir_plugin(self) -> bool {
        self.protects_stores()
    }

    pub fn protects_functions(self) -> bool {
        matches!(self, BuildConf::FunctionProtection | BuildConf::Full)
    }

    pub fn protects_stores(self) -> bool {
        matches!(self, BuildConf::StoreProtection | BuildConf::Full)
    }

    pub fn runtime_only(self) -> bool {
        self == BuildConf::RuntimeOnly
    }

    pub fn uses_runtime(self) -> bool {
        self != BuildConf::Vanilla
    }
}

impl FromStr for BuildConf {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        match BuildConf::ALL.into_iter().find(|stage| stage.name() == s) {
            Some(stage) => Ok(stage),
            None => bail!(
                "unknown build configuration: '{}' (try one of {})",
                s,
                BuildConf::ALL.map(|c| c.name()).join(",")
            ),
        }
    }
}

impl fmt::Display for BuildConf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl BuildConf {
    pub const fn description(self) -> &'static str {
        match self {
            BuildConf::Vanilla => "no protection",
            BuildConf::RuntimeOnly => "runtime only (relocate_stack + libruntime), no pass",
            BuildConf::StoreProtection => "runtime + store masking",
            BuildConf::FunctionProtection => "runtime + function protection",
            BuildConf::Full => "runtime + store masking + function protection",
        }
    }
}

impl clap::ValueEnum for BuildConf {
    fn value_variants<'a>() -> &'a [Self] {
        &Self::ALL
    }

    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(PossibleValue::new(self.name()).help(self.description()))
    }
}

impl FromStr for NonEmptySet<BuildConf> {
    type Err = Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        if s.trim() == "all" {
            return NonEmptySet::try_from(BuildConf::ALL.to_vec());
        }
        let configurations = s
            .split(',')
            .map(|p| {
                let p = p.trim();
                <BuildConf as clap::ValueEnum>::from_str(p, false)
                    .map_err(|_| anyhow!("unknown configuration \"{p}\""))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        NonEmptySet::try_from(configurations).context("no configurations")
    }
}
