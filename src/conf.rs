use anyhow::{Error, bail};
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

#[derive(Clone)]
pub struct BuildList {
    head: BuildConf,
    tail: Vec<BuildConf>,
}

impl TryFrom<Vec<BuildConf>> for BuildList {
    type Error = Error;

    fn try_from(stage_list: Vec<BuildConf>) -> Result<Self, Self::Error> {
        BuildList::from_slice(&stage_list).ok_or_else(|| anyhow::anyhow!("no configurations"))
    }
}

impl BuildList {
    pub fn every() -> Self {
        let [head, rest @ ..] = BuildConf::ALL;
        let mut l = BuildList::single(head);
        rest.iter().for_each(|&s| l.push(s));
        l
    }

    fn from_slice(stages: &[BuildConf]) -> Option<Self> {
        let (&head, rest) = stages.split_first()?;
        let mut l = BuildList {
            head,
            tail: Vec::new(),
        };
        for &st in rest {
            l.push(st);
        }
        Some(l)
    }

    pub fn single(stage: BuildConf) -> Self {
        Self {
            head: stage,
            tail: vec![],
        }
    }

    pub fn contains(&self, stage: BuildConf) -> bool {
        self.head == stage || self.tail.contains(&stage)
    }

    pub fn first(&self) -> BuildConf {
        self.head
    }

    pub fn iter(&self) -> impl Iterator<Item = BuildConf> + '_ {
        std::iter::once(self.head).chain(self.tail.iter().copied())
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut BuildConf> + '_ {
        std::iter::once(&mut self.head).chain(self.tail.iter_mut())
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn len(&self) -> usize {
        1 + self.tail.len()
    }

    pub fn push(&mut self, stage: BuildConf) {
        if !self.contains(stage) {
            self.tail.push(stage);
        }
    }
}

impl FromStr for BuildList {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        if s.trim() == "all" {
            return Ok(BuildList::every());
        }
        let stages = s
            .split(',')
            .map(str::parse)
            .collect::<Result<Vec<BuildConf>, _>>()?;
        BuildList::try_from(stages)
    }
}
