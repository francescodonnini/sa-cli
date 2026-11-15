use crate::conf::BuildConf;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Write;

pub(crate) struct Tee(Option<File>);

impl Tee {
    pub(crate) fn new(file: Option<File>) -> Self {
        Self(file)
    }

    pub(crate) fn line(&mut self, s: impl AsRef<str>) {
        let s = s.as_ref();
        println!("{s}");
        if let Some(f) = &mut self.0 {
            // un errore di scrittura non deve interrompere i test: la riga è comunque a schermo
            let _ = writeln!(f, "{s}");
        }
    }

    pub(crate) fn compare(&mut self, failed: &BTreeMap<BuildConf, BTreeSet<String>>) {
        let Some(reference) = failed.get(&BuildConf::Vanilla) else {
            return;
        };
        self.line("\n== failing only in the configuration (vs vanilla)");
        for (conf, names) in failed.iter().filter(|(c, _)| **c != BuildConf::Vanilla) {
            let extra: Vec<&String> = names.difference(reference).collect();
            if extra.is_empty() {
                self.line(format!("   {conf}: none"));
                continue;
            }
            self.line(format!("   {conf}: {}", extra.len()));
            for n in extra.iter().take(30) {
                self.line(format!("     {n}"));
            }
        }
    }
}
