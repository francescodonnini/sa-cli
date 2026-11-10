use serde::Deserialize;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureParser {
    /// Solo codice d'uscita
    #[default]
    ExitCode,
    Pg,
    /// Riepilogo di prove (nginx)
    Prove,
    Tap,
}

/// Output di `make check` di PostgreSQL: pg_regress (TAP) e prove nello stesso log, divisi
/// in sezioni "# +++ <tipo> check in <cartella> +++". I nomi diventano "<cartella>/<test>".
pub(crate) fn pg(log: &str) -> Vec<String> {
    let mut dir = "";
    let mut v = Vec::new();
    for l in log.lines() {
        if let Some(rest) = l.strip_prefix("# +++ ") {
            if let Some((_, d)) = rest.trim_end_matches(" +++").split_once(" check in ") {
                dir = d.trim();
            }
            continue;
        }
        if let Some(n) = tap_line(l).or_else(|| prove_line(l)) {
            v.push(if dir.is_empty() { n } else { format!("{dir}/{n}") });
        }
    }
    sorted(v)
}

pub(crate) fn prove(log: &str) -> Vec<String> {
    sorted(log.lines().filter_map(prove_line).collect())
}

/// Riga di riepilogo di prove per un file fallito: "t/001_x.pl (Wstat: 256 ... Failed: 1)".
/// Il riepilogo compare anche per file senza errori (es. "`TODO passed`"): conta solo se il
/// processo è uscito con errore o se qualche test è fallito.
fn prove_line(l: &str) -> Option<String> {
    let (name, rest) = l.split_once(char::is_whitespace)?;
    if !(name.ends_with(".t") || name.ends_with(".pl")) {
        return None;
    }
    let rest = rest.trim_start();
    if !rest.starts_with("(Wstat") {
        return None;
    }
    // se il formato non si legge, meglio un falso positivo che un fallimento perso
    let wstat = num_after(rest, "Wstat:").unwrap_or(1);
    let failed = num_after(rest, "Failed:").unwrap_or(0);
    (wstat != 0 || failed != 0).then(|| name.trim_start_matches("./").to_string())
}

/// Il numero che segue `label` in `s` (es. "Wstat: 256" -> 256).
fn num_after(s: &str, label: &str) -> Option<u64> {
    let i = s.find(label)? + label.len();
    s[i..].trim_start().split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// Nomi dei test falliti in un formato TAP ("not ok N - nome ..."); accetta anche il "+"
/// che pg_regress usa per i test dei gruppi paralleli.
pub(crate) fn tap(log: &str) -> Vec<String> {
    sorted(log.lines().filter_map(tap_line).collect())
}

fn tap_line(l: &str) -> Option<String> {
    let rest = l.trim_start().strip_prefix("not ok")?;
    if rest.contains("# TODO") {
        return None;
    }
    let rest = rest.trim_start().trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(['-', '+']).unwrap_or(rest);
    rest.split_whitespace().next().map(str::to_string)
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}