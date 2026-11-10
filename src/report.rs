use anyhow::{Context, Result, bail};
use std::{fs, io};
use std::path::Path;

const COLUMNS: [&str; 22] = [
    "functions",
    "stores_seen",
    "instrumented",
    "already_masked",
    "skipped_constaddr",
    "skipped_knownbits",
    "skipped_domtree",
    "store_inst",
    "vector_stores",
    "atomic_rmw",
    "cmpxchg",
    "mem_intrinsics",
    "masked_store",
    "arch_specific",
    "scatter",
    "unhandled_writes",
    "inline_asm",
    "external_calls_may_write",
    "base_small_off",
    "distinct_bases",
    "non_zero_off",
    "inplace",
];

#[derive(Debug, Default, PartialEq)]
pub struct IrReport {
    pub files: u64,
    totals: [u64; COLUMNS.len()],
}

impl IrReport {
    pub fn read(path: &Path) -> Result<Option<IrReport>> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        let mut r = IrReport::default();
        for (i, line) in text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
        {
            // i campi del csv sono composti dalle statistiche (le cui metriche di riferimento sono
            // riportate in `COLUMNS`) più il nome del file corrispondente
            let f: Vec<&str> = line.split(',').collect();
            if f.len() != COLUMNS.len() + 1 {
                bail!(
                    "{}:{}: expected {} fields (file + {} counters), got {}: is COLUMNS in sync with SSReportPass?",
                    path.display(),
                    i + 1,
                    COLUMNS.len() + 1,
                    COLUMNS.len(),
                    f.len()
                );
            }
            for (j, v) in f[1..].iter().enumerate() {
                r.totals[j] += v.trim().parse::<u64>().with_context(|| {
                    format!(
                        "{}:{}: {} is not a number",
                        path.display(),
                        i + 1,
                        COLUMNS[j]
                    )
                })?;
            }

            r.files += 1;
        }
        Ok(Some(r))
    }

    pub fn summary(&self) -> String {
        let g = |n| self.get(n);
        let seen = g("stores_seen");
        let pct = |v: u64| {
            if seen > 0 {
                100.0 * v as f64 / seen as f64
            } else {
                0.0
            }
        };
        let masked = g("instrumented") + g("inplace");
        let mut s = format!(
            "IR pass: {} files, {} functions, {seen} stores: {masked} masked ({:.1}%; inplace {}), \
             already masked {}, skipped: constaddr {}, knownbits {}, domtree {}",
            self.files,
            g("functions"),
            pct(masked),
            g("inplace"),
            g("already_masked"),
            g("skipped_constaddr"),
            g("skipped_knownbits"),
            g("skipped_domtree"),
        );
        let accounted = masked
            + g("already_masked")
            + g("skipped_constaddr")
            + g("skipped_knownbits")
            + g("skipped_domtree");
        if accounted != seen {
            s += &format!("\n  WARNING: {seen} stores seen but {accounted} accounted");
        }
        let unprotected: Vec<String> = [
            ("unhandled intrinsics", "unhandled_writes"),
            ("inline asm", "inline_asm"),
            ("external calls that may write", "external_calls_may_write"),
        ]
        .iter()
        .filter(|(_, col)| g(col) > 0)
        .map(|(label, col)| format!("{label} {}", g(col)))
        .collect();
        if !unprotected.is_empty() {
            s += &format!("\n  not protected: {}", unprotected.join(", "));
        }
        s
    }

    /// Totale di una colonna. Il nome deve essere in COLUMNS.
    pub fn get(&self, name: &str) -> u64 {
        let i = COLUMNS.iter().position(|c| *c == name);
        self.totals[i.unwrap_or_else(|| panic!("unknown report column {name}"))]
    }
}
