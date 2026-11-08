use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;

#[derive(Debug, Default, PartialEq)]
pub struct IrReport {
    pub files: u64,
    pub functions: u64,
    pub stores: u64,
    pub masked: u64,
    pub skipped_constaddr: u64,
    pub skipped_knownbits: u64,
    pub skipped_domtree: u64,
}

impl IrReport {
    pub fn read(path: &Path) -> Result<Option<IrReport>> {
        if !path.exists() {
            return Ok(None);
        }

        let text =
            fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        let mut r = IrReport::default();
        for (i, line) in text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
        {
            let f: Vec<&str> = line.split(',').collect();
            if f.len() != 7 {
                bail!(
                    "{}:{}: expected 7 fields, got {}",
                    path.display(),
                    i + 1,
                    f.len()
                );
            }
            let n = |j: usize| -> Result<u64> {
                f[j].trim().parse().with_context(|| {
                    format!(
                        "{}:{}: field {} is not a number",
                        path.display(),
                        i + 1,
                        j + 1
                    )
                })
            };
            r.files += 1;
            r.functions += n(1)?;
            r.stores += n(2)?;
            r.masked += n(3)?;
            r.skipped_constaddr += n(4)?;
            r.skipped_knownbits += n(5)?;
            r.skipped_domtree += n(6)?;
        }
        Ok(Some(r))
    }

    pub fn summary(&self) -> String {
        let pct = if self.stores > 0 {
            100.0 * self.masked as f64 / self.stores as f64
        } else {
            0.0
        };
        format!(
            "IR pass: {} files, {} functions, {} stores, {} masked ({pct:.1}%), skipped: constaddr {}, knownbits {}, domtree {}",
            self.files,
            self.functions,
            self.stores,
            self.masked,
            self.skipped_constaddr,
            self.skipped_knownbits,
            self.skipped_domtree
        )
    }
}
