use crate::conf::BuildList;
use crate::report::{Entry, Report, Status};
use crate::toolchain::Toolchain;
use anyhow::{Context, Result, bail};
use std::fs;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::process::{Command, Stdio};

pub fn run(tc: &Toolchain, builds: &BuildList, quick: bool) -> Result<()> {
    let report = checks(tc, builds, quick);
    report.print(false);
    if report.failed() {
        bail!("missing prerequisites");
    }
    Ok(())
}

pub fn require(tc: &Toolchain, builds: &BuildList) -> Result<()> {
    let report = checks(tc, builds, true);
    if report.failed() {
        report.print(true);
        bail!("missing prerequisites (run `doctor` for details)");
    }
    Ok(())
}

pub fn checks(tc: &Toolchain, builds: &BuildList, quick: bool) -> Report {
    let mut report = Report::default();
    for tool in ["objdump", "nm", "readelf", "ldd"] {
        report.push(check_version(tool));
    }
    let version = report.check("clang", clang_version(&tc.clang), |v| {
        format!("{} ({v})", tc.clang.display())
    });
    let mut options: Vec<&str> = builds.iter().flat_map(|c| c.backend_options()).collect();
    options.sort();
    options.dedup();
    for opt in options {
        let name = format!("clang accepts {opt}");
        if version.is_some() {
            report.check(&name, accepts_backend_option(&tc.clang, opt), |_| {
                String::new()
            });
        } else {
            report.skip(&name, "clang not available");
        }
    }
    if builds
        .iter()
        .any(|c| c.uses_runtime() || c.protects_stores())
    {
        report.check("4-level paging", four_level_paging(), |s| s.clone());
    }
    if builds.iter().any(|c| c.uses_runtime()) {
        let stckcpy = tc.runtime.join("stckcpy.o");
        let libruntime = tc.runtime.join("libruntime.so");
        let device = Path::new("/dev/shdwstck");
        report.check("runtime: stckcpy.o", regular_file(&stckcpy), |_| {
            stckcpy.display().to_string()
        });
        report.check("runtime: libruntime.so", regular_file(&libruntime), |_| {
            libruntime.display().to_string()
        });
        report.check("kernel module", shadow_device(device), |_| {
            device.display().to_string()
        });
    }
    report
}

fn check_version(tool: &'static str) -> Entry {
    let detail = match Command::new(tool).arg("--version").output() {
        Ok(out) if out.status.success() => {
            let text = if out.stdout.is_empty() {
                &out.stderr
            } else {
                &out.stdout
            };
            let first = String::from_utf8_lossy(text)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            return Entry {
                status: Status::Ok,
                name: tool.into(),
                detail: first,
            };
        }
        Ok(out) => format!("{tool} --version failed ({})", out.status),
        Err(e) if e.kind() == ErrorKind::NotFound => format!("{tool} not found in PATH"),
        Err(e) => format!("cannot run {tool}: {e}"),
    };
    Entry {
        status: Status::Fail,
        name: tool.into(),
        detail,
    }
}

fn clang_version(clang: &Path) -> Result<String> {
    let out = match Command::new(clang).arg("--version").output() {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => bail!("{} not found", clang.display()),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            bail!("{} is not executable", clang.display())
        }
        Err(e) => return Err(e).with_context(|| format!("cannot run {}", clang.display())),
    };
    if !out.status.success() {
        bail!(
            "{} --version exited with non-zero status code {}",
            clang.display(),
            out.status
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.lines().next().unwrap_or("").trim().to_string())
}

fn accepts_backend_option(clang: &Path, option: &str) -> Result<()> {
    let out = Command::new(clang)
        .args(["-mllvm", option, "-x", "c", "-c", "-", "-o", "/dev/null"])
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("cannot run {}", clang.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("Unknown command line argument") {
            bail!("{} doesn't know {option}", clang.display());
        }
        bail!("compilation test failed: {}", err.trim());
    }
    Ok(())
}

fn four_level_paging() -> Result<String> {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").context("cannot read /proc/cpuinfo")?;
    match virtual_address_bits(&cpuinfo) {
        Some(48) => Ok("4-level (48-bit virtual addresses)".to_string()),
        Some(57) => {
            let hint = (1usize << 50) as *mut libc::c_void;
            let len = 4096;
            let p = unsafe {
                libc::mmap(
                    hint,
                    len,
                    libc::PROT_NONE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if p == libc::MAP_FAILED {
                bail!("mmap failed: {}", std::io::Error::last_os_error());
            }
            unsafe { libc::munmap(p, len) };
            if p as usize >= 1 << 47 {
                bail!(
                    "5-level paging is active: bit-47 masking is not valid (boot the kernel with no5lvl)"
                );
            }
            Ok("4-level (LA57 supported but not active)".into())
        }
        _ => bail!("cannot determine paging level"),
    }
}

fn virtual_address_bits(cpuinfo: &str) -> Option<u32> {
    let line = cpuinfo.lines().find(|l| l.starts_with("address sizes"))?;
    let virt = line.split(',').find(|p| p.contains("virtual"))?;
    virt.split_whitespace().next()?.parse().ok()
}

fn regular_file(p: &Path) -> Result<()> {
    match p.metadata() {
        Ok(m) if m.is_file() => Ok(()),
        Ok(_) => bail!("{} is not a regular file", p.display()),
        Err(e) if e.kind() == ErrorKind::NotFound => bail!("{} not found", p.display()),
        Err(e) => Err(e).with_context(|| format!("cannot access {}", p.display())),
    }
}

fn shadow_device(p: &Path) -> Result<()> {
    let meta = match p.metadata() {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            bail!("{} not found: the kernel module is not loaded", p.display());
        }
        Err(e) => return Err(e).with_context(|| format!("cannot access {}", p.display())),
    };
    if !meta.file_type().is_char_device() {
        bail!("{} exists but is not a char device", p.display());
    }
    match OpenOptions::new().read(true).write(true).open(p) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            bail!(
                "{} is not accessible by the current user (check permissions or udev rule)",
                p.display()
            )
        }
        Err(e) => Err(e).with_context(|| format!("cannot open {}", p.display())),
    }
}
