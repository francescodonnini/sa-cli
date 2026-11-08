use crate::conf::BuildConf;
use std::env;
use std::path::PathBuf;

pub struct Toolchain {
    pub targets_root: PathBuf,
    pub results: PathBuf,
    pub clang: PathBuf,
    pub runtime: PathBuf,
    pub ir_pass: PathBuf,
    pub programs: PathBuf,
}

impl Toolchain {
    pub fn from_env() -> Self {
        let home = PathBuf::from(env::var_os("HOME").unwrap_or_default());
        Self {
            targets_root: Self::get_path("SS_ROOT", home.join("ss")),
            results: Self::get_path("SS_RESULTS", home.join("results")),
            clang: Self::get_path("CLANG", home.join("llvm-project/build/bin/clang")),
            runtime: Self::get_path("RT", home.join("runtime/build")),
            ir_pass: Self::get_path("IR_PASS", home.join("shadow_stack/build/ShadowStack.so")),
            programs: Self::get_path(
                "SS_PROGRAMS",
                PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/programs")),
            ),
        }
    }

    fn get_path(var: &str, default: PathBuf) -> PathBuf {
        env::var_os(var).map(PathBuf::from).unwrap_or(default)
    }

    pub fn clangxx(&self) -> PathBuf {
        let mut name = self.clang.file_name().unwrap_or_default().to_os_string();
        name.push("++");
        self.clang.with_file_name(name)
    }

    pub fn compile_flags(&self, conf: BuildConf) -> Vec<String> {
        let mut v = Vec::new();
        if conf.needs_ir_plugin() {
            v.push(format!("-fpass-plugin={}", self.ir_pass.display()));
        }
        for opt in conf.backend_options() {
            v.push("-mllvm".into());
            v.push(opt.into());
        }
        v
    }

    pub fn link_flags(&self, conf: BuildConf) -> Vec<String> {
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
