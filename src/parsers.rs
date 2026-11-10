use serde::Deserialize;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureParser {
    /// Solo codice d'uscita
    #[default]
    ExitCode,
    /// Riepilogo di prove (nginx)
    Prove,
}

pub(crate) fn prove(log: &str) -> Vec<String> {
    let mut v: Vec<String> = log
        .lines()
        .filter_map(|l| {
            let (name, rest) = l.split_once(char::is_whitespace)?;
            let failed = name.ends_with(".t") && rest.trim_start().starts_with("(Wstat");
            failed.then(|| name.trim_start_matches("./").to_string())
        })
        .collect();
    v.sort();
    v.dedup();
    v
}
