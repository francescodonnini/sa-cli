use anyhow::Result;
use std::path::Path;

type BenchParser = fn(stdout: &str) -> Result<Vec<(String, f64)>>;
type FailureParser = fn(log: &str, out: &Path) -> Result<Vec<String>>;