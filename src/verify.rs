use crate::conf::BuildConf;
use crate::toolchain::Toolchain;
use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

const SHADOW_OFFSET: &str = "__ss_shadow_offset";
const ENTRY: &str = "relocate_stack";

pub fn binary(tc: &Toolchain, conf: BuildConf, bin: &Path, min_refs: usize) -> Result<String> {
    if !bin.is_file() {
        bail!("{} not found", bin.display());
    }

    let refs = shadow_offset_refs(bin)?;
    if conf.protects_functions() {
        if refs < min_refs {
            bail!(
                "{}: {refs} references to {SHADOW_OFFSET}, expected at least {min_refs} refs",
                bin.display()
            );
        }
    } else if conf.uses_runtime() {
        if refs >= min_refs {
            bail!(
                "{}: {refs} references to {SHADOW_OFFSET}: contains the x86 instrumentation",
                bin.display()
            );
        }
    } else if refs > 0 {
        bail!(
            "{}: {refs} references to {SHADOW_OFFSET} in a vanilla build",
            bin.display()
        );
    }

    let reloc = symbol_address(bin, ENTRY)?;
    if conf.uses_runtime() {
        let Some(reloc) = reloc else {
            bail!("{}: {ENTRY} not found", bin.display());
        };
        let entry = entry_point(bin)?;
        if entry != reloc {
            bail!(
                "{}: entry point {entry:#x} is not {ENTRY} ({reloc:#x})",
                bin.display()
            );
        }
        let rt = tc.runtime.join("libruntime.so");
        if !resolves(bin, &rt)? {
            bail!(
                "{}: libruntime.so is not resolved from {}",
                bin.display(),
                tc.runtime.display()
            );
        }
    } else if reloc.is_some() {
        bail!("{}: {ENTRY} present in a vanilla build", bin.display());
    }
    Ok(format!(
        "{}: {refs} references to {SHADOW_OFFSET}",
        bin.display()
    ))
}

fn shadow_offset_refs(bin: &Path) -> Result<usize> {
    let mut child = Command::new("objdump")
        .args(["-d", "--no-show-raw-insn"])
        .arg(bin)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("cannot run objdump")?;
    let out = child.stdout.take().context("objdump without stdout")?;
    let n = BufReader::new(out)
        .lines()
        .map_while(Result::ok)
        .filter(|l| l.contains(SHADOW_OFFSET))
        .count();
    let status = child.wait()?;
    if !status.success() {
        bail!("objdump -d {} failed ({status})", bin.display());
    }
    Ok(n)
}

fn symbol_address(bin: &Path, name: &str) -> Result<Option<u64>> {
    let out = Command::new("nm")
        .arg(bin)
        .output()
        .context("cannot run nm")?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if let [addr, _kind, sym] = f[..] {
            if sym == name {
                return Ok(Some(u64::from_str_radix(addr, 16)?));
            }
        }
    }
    Ok(None)
}

fn entry_point(bin: &Path) -> Result<u64> {
    let out = Command::new("readelf")
        .arg("-h")
        .arg(bin)
        .output()
        .context("cannot run readelf")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let addr = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("Entry point address:"))
        .with_context(|| format!("{}: no entry point in readelf -h", bin.display()))?;
    Ok(u64::from_str_radix(
        addr.trim().trim_start_matches("0x"),
        16,
    )?)
}

fn resolves(bin: &Path, lib: &Path) -> Result<bool> {
    let out = Command::new("ldd")
        .arg(bin)
        .output()
        .context("cannot run ldd")?;
    let text = String::from_utf8_lossy(&out.stdout);
    let lib = lib.display().to_string();
    Ok(text.lines().any(|l| {
        l.split("=>")
            .nth(1)
            .is_some_and(|r| r.trim_start().starts_with(&lib))
    }))
}
