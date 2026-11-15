use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::cmp::PartialEq;
use std::collections::BTreeSet;
use std::time::Duration;

/// Come ricavare i valori di una misura di benchmark.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum MetricParser {
    /// Tempo di esecuzione del comando, misurato da btp: solo per i casi single (in un caso
    /// client-server sarebbe la durata del client)
    #[default]
    Time,
    /// wrk: "Requests/sec:"; errore con risposte non 2xx/3xx o errori di socket
    Wrk,
    /// `redis-benchmark --csv`: rps, p50 e p99 per ogni comando
    RedisBenchmark,
    /// pgbench: tps e latenza media; errore con transazioni fallite
    Pgbench,
    /// runcpu di SPEC CPU 2017: il tempo di ogni benchmark, dai CSV che runcpu indica
    /// nell'output ("format: CSV -> ...")
    Spec,
}

/// Verso di un valore: serve al riepilogo per calcolare l'overhead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Better {
    Lower,
    Higher,
}

impl Better {
    pub fn as_str(self) -> &'static str {
        match self {
            Better::Lower => "lower",
            Better::Higher => "higher",
        }
    }
}

/// Un valore prodotto da una misura. Una misura può produrne più d'uno (es. un valore per
/// ogni comando di redis-benchmark).
#[derive(Debug, Clone)]
pub struct Sample {
    pub metric: String,
    pub value: f64,
    pub unit: &'static str,
    pub better: Better,
}

impl Sample {
    fn new(metric: impl Into<String>, value: f64, unit: &'static str, better: Better) -> Sample {
        Sample {
            metric: metric.into(),
            value,
            unit,
            better,
        }
    }
}

impl MetricParser {
    /// Vero se i valori vengono dall'output del comando, e non dal tempo misurato da btp.
    pub fn reads_output(self) -> bool {
        self != MetricParser::Time
    }

    /// `raw` è l'output del comando misurato, `elapsed` la sua durata.
    pub(crate) fn parse(self, raw: &str, elapsed: Duration) -> Result<Vec<Sample>> {
        let clean = strip_ansi(raw);
        let log = clean.as_str();
        match self {
            MetricParser::Time => Ok(vec![Sample::new(
                "time",
                elapsed.as_secs_f64(),
                "s",
                Better::Lower,
            )]),
            MetricParser::Wrk => wrk(log),
            MetricParser::RedisBenchmark => redis_benchmark(log),
            MetricParser::Pgbench => pgbench(log),
            MetricParser::Spec => spec_times(log),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum FailureParser {
    Catch2,
    Cpython,
    Ctest,
    /// Solo codice d'uscita
    #[default]
    ExitCode,
    /// `make check` di PostgreSQL: pg_regress e prove, divisi in sezioni per cartella
    Pg,
    Php,
    /// Riepilogo di prove (nginx)
    Prove,
    Redis,
    Spec,
    Sqlite,
    /// Output TAP (pg_regress)
    Tap,
}

/// Conteggi di una suite. L'unità dipende dal formato: un test per TAP (pg_regress),
/// un file per prove.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct Counts {
    pub total: usize,
    pub failed: usize,
    pub skipped: usize,
}

impl Counts {
    pub fn passed(&self) -> usize {
        self.total.saturating_sub(self.failed + self.skipped)
    }
}

#[derive(Debug, Default)]
pub(crate) struct Outcome {
    /// `None` se il formato non permette di contare (exit-code)
    pub counts: Option<Counts>,
    /// Nomi dei test falliti, ordinati e senza duplicati
    pub failed: Vec<String>,
}

impl FailureParser {
    pub(crate) fn parse(self, raw: &str) -> Outcome {
        let clean = strip_ansi(raw);
        let log = clean.as_str();
        let (tap, prove, sections) = match self {
            FailureParser::Catch2 => return catch2(log),
            FailureParser::Cpython => return cpython(log),
            FailureParser::Ctest => return ctest(log),
            FailureParser::ExitCode => return Outcome::default(),
            FailureParser::Pg => (true, true, true),
            FailureParser::Php => return php(log),
            FailureParser::Prove => (false, true, false),
            FailureParser::Redis => return redis(log),
            FailureParser::Spec => return spec(log),
            FailureParser::Sqlite => return sqlite(log),
            FailureParser::Tap => (true, false, false),
        };
        let mut dir = "";
        let mut c = Counts::default();
        let mut failed = Vec::new();
        for l in log.lines() {
            if sections {
                if let Some(d) = section(l) {
                    dir = d;
                    continue;
                }
            }
            if tap {
                if let Some((name, st)) = tap_result(l) {
                    c.total += 1;
                    match st {
                        Status::Failed => {
                            c.failed += 1;
                            failed.push(qualify(dir, name));
                        }
                        Status::Skipped => c.skipped += 1,
                        Status::Passed => {}
                    }
                    continue;
                }
            }
            if prove {
                if let Some(n) = prove_files(l) {
                    c.total += n;
                } else if prove_skipped(l) {
                    c.skipped += 1;
                } else if let Some(name) = prove_failed(l) {
                    c.failed += 1;
                    failed.push(qualify(dir, name));
                }
            }
        }
        failed.sort();
        failed.dedup();
        Outcome {
            counts: Some(c),
            failed,
        }
    }
}

enum Status {
    Passed,
    Failed,
    Skipped,
}

/// regrtest di CPython: "N tests OK.", "Total test files: run=A/B failed=F ..." e l'elenco
/// rientrato sotto "N tests failed:". Tutto ciò che non è passato né fallito (saltati,
/// risorse negate, file senza test) conta come saltato. Senza riepilogo, regrtest non è
/// arrivato alla fine.
fn cpython(log: &str) -> Outcome {
    let mut total: Option<usize> = None;
    let mut failed_n = 0;
    let mut ok = 0;
    let mut failed = Vec::new();
    let mut in_list = false;
    for l in log.lines() {
        if in_list {
            if l.starts_with(char::is_whitespace) && !l.trim().is_empty() {
                failed.extend(l.split_whitespace().map(str::to_string));
                continue;
            }
            in_list = false;
        }
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("Total test files:") {
            for kv in rest.split_whitespace() {
                match kv.split_once('=') {
                    Some(("run", v)) => {
                        total = v
                            .split('/')
                            .nth(1)
                            .and_then(|n| n.replace(',', "").parse().ok())
                    }
                    Some(("failed", v)) => failed_n = v.replace(',', "").parse().unwrap_or(0),
                    _ => {}
                }
            }
        } else if let Some(n) = t
            .strip_suffix(" tests OK.")
            .or_else(|| t.strip_suffix(" test OK."))
        {
            ok = n
                .trim_start_matches("All ")
                .replace(',', "")
                .parse()
                .unwrap_or(0);
        } else if t.ends_with(" failed:") && t.starts_with(|c: char| c.is_ascii_digit()) {
            in_list = true;
        }
    }
    let Some(total) = total else {
        return Outcome {
            counts: None,
            failed: vec!["(no summary: regrtest did not finish)".into()],
        };
    };
    failed.sort();
    failed.dedup();
    let skipped = total.saturating_sub(ok + failed_n);
    Outcome {
        counts: Some(Counts {
            total,
            failed: failed_n,
            skipped,
        }),
        failed,
    }
}

/// Catch2 (duckdb unittest): "All tests passed (A assertions in N test cases)" oppure
/// "test cases: N | P passed | F failed". Il nome di un caso fallito sta tra due righe di
/// trattini, nell'intestazione che il reporter stampa per ogni fallimento.
fn catch2(log: &str) -> Outcome {
    fn nums(s: &str) -> Vec<usize> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter_map(|n| n.parse().ok())
            .collect()
    }
    let lines: Vec<&str> = log.lines().collect();
    let dashes = |l: &str| l.len() >= 20 && l.chars().all(|c| c == '-');
    let mut failed = Vec::new();
    for w in lines.windows(3) {
        if dashes(w[0]) && dashes(w[2]) && !w[1].trim().is_empty() {
            failed.push(w[1].trim().to_string());
        }
    }
    let mut counts = None;
    for l in &lines {
        if let Some(rest) = l.strip_prefix("All tests passed") {
            // "(A assertions in N test cases)": N è il secondo numero
            if let Some(&n) = nums(rest).get(1) {
                counts = Some(Counts {
                    total: n,
                    failed: 0,
                    skipped: 0,
                });
            }
        } else if let Some(rest) = l.strip_prefix("test cases:") {
            // "T | P passed | F failed | S skipped": il primo campo è il totale, gli altri
            // si riconoscono dall'etichetta (l'ordine e la presenza variano tra le versioni)
            let mut c = Counts::default();
            for (i, field) in rest.split('|').enumerate() {
                let mut words = field.split_whitespace();
                let n = words.next().and_then(|n| n.parse().ok()).unwrap_or(0);
                match (i, words.next()) {
                    (0, _) => c.total = n,
                    (_, Some("failed")) => c.failed = n,
                    (_, Some("skipped")) => c.skipped = n,
                    _ => {}
                }
            }
            counts = Some(c);
        }
    }
    let Some(c) = counts else {
        return Outcome {
            counts: None,
            failed: vec!["(no summary: unittest did not finish)".into()],
        };
    };
    failed.sort();
    failed.dedup();
    Outcome {
        counts: Some(c),
        failed,
    }
}

/// `testrunner.tcl` di SQLite: "E errors out of T tests in 00:35 on ...". I dettagli dei
/// fallimenti stanno in testrunner.log (righe "!"), non nell'output. Senza riepilogo,
/// testrunner non è arrivato alla fine: conta come fallimento.
fn sqlite(log: &str) -> Outcome {
    let summary = log.lines().rev().find_map(|l| {
        let (e, rest) = l.split_once(" errors out of ")?;
        let t = rest.split_whitespace().next()?;
        Some((e.trim().parse::<usize>().ok()?, t.parse::<usize>().ok()?))
    });
    match summary {
        Some((errors, total)) => Outcome {
            counts: Some(Counts {
                total,
                failed: errors,
                skipped: 0,
            }),
            failed: if errors > 0 {
                vec![format!("{errors} errors (see testrunner.log)")]
            } else {
                Vec::new()
            },
        },
        None => Outcome {
            counts: None,
            failed: vec!["(no summary: testrunner did not finish)".into()],
        },
    }
}

/// `runtest` di Redis: una riga per test, "[ok]: nome (N ms)", "[err]: nome in file",
/// "[skip]: nome"; "[exception]: ..." è un crash del client o del server.
/// Il riepilogo finale ("*** [err]: ...") non inizia con "[" e non viene contato.
fn redis(log: &str) -> Outcome {
    let mut c = Counts::default();
    let mut failed = Vec::new();
    for l in log.lines() {
        let Some((tag, rest)) = l.strip_prefix('[').and_then(|r| r.split_once("]: ")) else {
            continue;
        };
        match tag {
            "ok" => c.total += 1,
            "skip" | "ignore" => {
                c.total += 1;
                c.skipped += 1;
            }
            "err" | "exception" => {
                c.total += 1;
                c.failed += 1;
                failed.push(rest.trim().to_string());
            }
            _ => {}
        }
    }
    failed.sort();
    failed.dedup();
    Outcome {
        counts: Some(c),
        failed,
    }
}

/// run-tests.php di PHP: tabella di riepilogo ("Number of tests : T E", "Tests failed : N
/// (...)") e sezioni "FAILED TEST SUMMARY" / "BORKED TEST SUMMARY" con una riga per test,
/// "titolo [percorso.phpt]". Senza riepilogo, run-tests non è arrivato alla fine.
fn php(log: &str) -> Outcome {
    /// Il primo numero dopo i due punti: "Tests failed    :     3 (  0.0%)" -> 3
    fn first_num(l: &str) -> Option<usize> {
        l.split_once(':')?.1.split_whitespace().next()?.parse().ok()
    }
    let mut c = Counts::default();
    let mut summary = false;
    let mut failed = Vec::new();
    let mut in_list = false;
    for l in log.lines() {
        let key = l.split(':').next().unwrap_or("").trim();
        match key {
            "Number of tests" => {
                c.total = first_num(l).unwrap_or(0);
                summary = true;
                continue;
            }
            "Tests skipped" => c.skipped = first_num(l).unwrap_or(0),
            "Tests failed" | "Tests borked" => c.failed += first_num(l).unwrap_or(0),
            _ => {}
        }
        if l.starts_with("FAILED TEST SUMMARY") || l.starts_with("BORKED TEST SUMMARY") {
            in_list = true;
            continue;
        }
        if in_list {
            if l.starts_with("=====") {
                in_list = false;
            } else if let Some((_, rest)) = l.rsplit_once('[') {
                if let Some(path) = rest.strip_suffix(']').filter(|p| p.ends_with(".phpt")) {
                    failed.push(path.to_string());
                }
            }
        }
    }
    if !summary {
        return Outcome {
            counts: None,
            failed: vec!["(no summary: run-tests did not finish)".into()],
        };
    }
    failed.sort();
    failed.dedup();
    Outcome {
        counts: Some(c),
        failed,
    }
}

/// Riepilogo di ctest: "N% tests passed, F tests failed out of T", poi gli elenchi
/// "The following tests FAILED:" e "The following tests did not run:".
fn ctest(log: &str) -> Outcome {
    let mut c = Counts::default();
    let mut failed = Vec::new();
    // Some(true): elenco dei falliti, Some(false): elenco dei non eseguiti
    let mut block = None;
    for l in log.lines() {
        if let Some((_, n)) = l.split_once("tests failed out of ") {
            if let Ok(n) = n.trim().parse() {
                c.total = n;
            }
            continue;
        }
        if l.starts_with("The following tests FAILED:") {
            block = Some(true);
            continue;
        }
        if l.starts_with("The following tests did not run:") {
            block = Some(false);
            continue;
        }
        match (block, ctest_entry(l)) {
            (Some(true), Some(name)) => {
                c.failed += 1;
                failed.push(name);
            }
            (Some(false), Some(_)) => c.skipped += 1,
            _ => block = None,
        }
    }
    failed.sort();
    failed.dedup();
    Outcome {
        counts: Some(c),
        failed,
    }
}

/// "\t  5 - test_index (Failed)" -> "test_index"
fn ctest_entry(l: &str) -> Option<String> {
    let (num, rest) = l.trim_start().split_once(" - ")?;
    num.parse::<u32>().ok()?;
    let name = rest.rsplit_once(" (").map_or(rest, |(n, _)| n);
    Some(name.trim().to_string())
}

fn qualify(dir: &str, name: String) -> String {
    if dir.is_empty() {
        name
    } else {
        format!("{dir}/{name}")
    }
}

/// "# +++ regress check in contrib/amcheck +++" -> "contrib/amcheck"
fn section(l: &str) -> Option<&str> {
    let rest = l.strip_prefix("# +++ ")?.strip_suffix(" +++")?;
    rest.split_once(" check in ").map(|(_, d)| d.trim())
}

/// Riga TAP di primo livello: "ok N - nome", "not ok N + nome" (gruppi paralleli di
/// pg_regress). "# SKIP" conta come saltato, un "not ok # TODO" come passato (fallimento
/// previsto).
fn tap_result(l: &str) -> Option<(String, Status)> {
    let (ok, rest) = match l.strip_prefix("not ok") {
        Some(r) => (false, r),
        None => (true, l.strip_prefix("ok")?),
    };
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let directive = rest
        .split_once('#')
        .map(|(_, d)| d.trim_start().to_ascii_lowercase());
    let skip = directive.as_deref().is_some_and(|d| d.starts_with("skip"));
    let todo = directive.as_deref().is_some_and(|d| d.starts_with("todo"));
    let rest = rest
        .trim_start()
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start();
    let rest = rest.strip_prefix(['-', '+']).unwrap_or(rest);
    let name = rest.split_whitespace().next().unwrap_or("").to_string();
    let st = if skip {
        Status::Skipped
    } else if ok || todo {
        Status::Passed
    } else {
        Status::Failed
    };
    Some((name, st))
}

fn is_test_file(name: &str) -> bool {
    name.ends_with(".t") || name.ends_with(".pl")
}

/// "Files=5, Tests=233, ..." -> 5 (file eseguiti da una invocazione di prove)
fn prove_files(l: &str) -> Option<usize> {
    l.strip_prefix("Files=")?
        .split(',')
        .next()?
        .trim()
        .parse()
        .ok()
}

/// `"t/010_tab_completion.pl .. skipped: readline is not supported"`
fn prove_skipped(l: &str) -> bool {
    let Some((name, rest)) = l.split_once(' ') else {
        return false;
    };
    is_test_file(name)
        && rest.starts_with("..")
        && rest.trim_start_matches(['.', ' ']).starts_with("skipped")
}

/// Il numero che segue `label` in `s` (es. "Wstat: 256" -> 256).
fn num_after(s: &str, label: &str) -> Option<u64> {
    let i = s.find(label)? + label.len();
    s[i..]
        .trim_start()
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// Riga del "Test Summary Report" di prove per un file fallito. Il riepilogo compare anche
/// senza errori (es. "TODO passed"): conta solo se il processo è uscito con errore o se
/// qualche test è fallito.
fn prove_failed(l: &str) -> Option<String> {
    let (name, rest) = l.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    if !is_test_file(name) || !rest.starts_with("(Wstat") {
        return None;
    }
    // se il formato non si legge, meglio un falso positivo che un fallimento perso
    let wstat = num_after(rest, "Wstat:").unwrap_or(1);
    let failed = num_after(rest, "Failed:").unwrap_or(0);
    (wstat != 0 || failed != 0).then(|| name.trim_start_matches("./").to_string())
}

/// Rimuove le sequenze di escape ANSI (colori, stili): ESC '[' parametri lettera-finale.
/// Un ESC non seguito da '[' viene scartato insieme al carattere successivo.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        if chars.next() == Some('[') {
            // la sequenza termina con un carattere tra '@' e '~'
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

/// wrk: "Requests/sec:  12345.67". Una misura con risposte non 2xx/3xx o errori di socket non
/// vale: il server ha risposto male o ha chiuso connessioni.
fn wrk(log: &str) -> Result<Vec<Sample>> {
    let mut rps = None;
    for line in log.lines().map(str::trim) {
        if let Some(n) = line.strip_prefix("Non-2xx or 3xx responses:") {
            bail!("wrk: {} non-2xx/3xx responses", n.trim());
        }
        if line.starts_with("Socket errors:") {
            bail!("wrk: {line}");
        }
        if let Some(v) = line.strip_prefix("Requests/sec:") {
            rps = Some(v.trim().parse::<f64>().context("wrk: bad Requests/sec")?);
        }
    }
    let rps = rps.context("wrk: no Requests/sec line")?;
    Ok(vec![Sample::new("rps", rps, "req/s", Better::Higher)])
}

/// `redis-benchmark --csv`: un'intestazione e una riga per comando, tutti i campi tra virgolette:
/// "test","rps","avg_latency_ms","min_latency_ms","p50_latency_ms","p95_latency_ms",...
fn redis_benchmark(log: &str) -> Result<Vec<Sample>> {
    let split = |l: &str| -> Vec<String> {
        l.split(',')
            .map(|f| f.trim().trim_matches('"').to_string())
            .collect()
    };
    let mut lines = log.lines().filter(|l| l.starts_with('"'));
    let header = split(lines.next().context("redis-benchmark: no CSV header")?);
    let col = |name: &str| {
        header
            .iter()
            .position(|h| h == name)
            .with_context(|| format!("redis-benchmark: no column {name}"))
    };
    let (test, rps, p50, p99) = (
        col("test")?,
        col("rps")?,
        col("p50_latency_ms")?,
        col("p99_latency_ms")?,
    );
    let mut out = Vec::new();
    for line in lines {
        let f = split(line);
        // "LRANGE_100 (first 100 elements)" -> "LRANGE_100"
        let name = f[test].split_whitespace().next().unwrap_or_default();
        let num = |i: usize| -> Result<f64> {
            f.get(i)
                .and_then(|v| v.parse().ok())
                .with_context(|| format!("redis-benchmark: bad value in {line:?}"))
        };
        out.push(Sample::new(
            format!("{name}.rps"),
            num(rps)?,
            "req/s",
            Better::Higher,
        ));
        out.push(Sample::new(
            format!("{name}.p50"),
            num(p50)?,
            "ms",
            Better::Lower,
        ));
        out.push(Sample::new(
            format!("{name}.p99"),
            num(p99)?,
            "ms",
            Better::Lower,
        ));
    }
    if out.is_empty() {
        bail!("redis-benchmark: no results");
    }
    Ok(out)
}

/// pgbench: "latency average = 0.648 ms", "tps = 12345.678901 (without initial connection
/// time)" e "number of failed transactions: 0 (0.000%)". Una misura con transazioni fallite
/// (errori SQL, connessioni perse) non vale.
fn pgbench(log: &str) -> Result<Vec<Sample>> {
    let (mut tps, mut latency) = (None, None);
    for l in log.lines().map(str::trim) {
        if let Some(rest) = l.strip_prefix("number of failed transactions:") {
            let n: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(1);
            if n != 0 {
                bail!("pgbench: {n} failed transactions");
            }
        } else if let Some(rest) = l.strip_prefix("tps = ") {
            let v = rest.split_whitespace().next().unwrap_or_default();
            tps = Some(v.parse::<f64>().context("pgbench: bad tps")?);
        } else if let Some(rest) = l.strip_prefix("latency average = ") {
            let v = rest
                .strip_suffix(" ms")
                .context("pgbench: latency not in ms")?;
            latency = Some(v.trim().parse::<f64>().context("pgbench: bad latency")?);
        }
    }
    let tps = tps.context("pgbench: no tps line")?;
    let mut out = vec![Sample::new("tps", tps, "tx/s", Better::Higher)];
    if let Some(l) = latency {
        out.push(Sample::new("latency", l, "ms", Better::Lower));
    }
    Ok(out)
}

/// runcpu scrive i risultati in un CSV per suite (intrate, fprate) e ne stampa il percorso:
/// "    format: CSV -> <output_root>/result/CPU2017.003.intrate.refrate.csv". Si leggono solo
/// quei file, non altri CSV rimasti da esecuzioni precedenti.
fn spec_times(log: &str) -> Result<Vec<Sample>> {
    let mut out = Vec::new();
    let mut files = 0;
    for l in log.lines() {
        let Some(path) = l.trim().strip_prefix("format: CSV -> ") else {
            continue;
        };
        let csv = std::fs::read_to_string(path.trim())
            .with_context(|| format!("spec: cannot read {}", path.trim()))?;
        out.extend(spec_csv(&csv).with_context(|| format!("spec: {}", path.trim()))?);
        files += 1;
    }
    if files == 0 {
        bail!("spec: no CSV in the runcpu output");
    }
    if out.is_empty() {
        bail!("spec: no benchmark results in the CSV");
    }
    Ok(out)
}

/// La tabella "Selected Results Table" di un CSV di runcpu: per ogni benchmark il tempo
/// dell'iterazione selezionata (la mediana se --iterations > 1).
/// Colonne: Benchmark, "Base # Copies", "Est. Base Run Time", "Est. Base Rate",
/// "Base Selected", "Base Status", ...; stato S = riuscito e validato, NR = non eseguito.
/// Qualsiasi altro stato (RE, VE, CE, ...) rende la misura non valida.
fn spec_csv(csv: &str) -> Result<Vec<Sample>> {
    let mut lines = csv
        .lines()
        .skip_while(|l| l.trim() != "\"Selected Results Table\"")
        .skip(1)
        .skip_while(|l| l.trim().is_empty());
    let header: Vec<&str> = lines
        .next()
        .context("no Selected Results Table")?
        .split(',')
        .map(|h| h.trim_matches('"'))
        .collect();
    let col = |name: &str| {
        header
            .iter()
            .position(|h| *h == name)
            .with_context(|| format!("no column {name}"))
    };
    let (time, status) = (col("Est. Base Run Time")?, col("Base Status")?);
    let mut out = Vec::new();
    let mut bad = Vec::new();
    for l in lines.take_while(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = l.split(',').collect();
        let name = f[0];
        match f.get(status).copied() {
            Some("NR") => continue,
            Some("S") => {
                let t = f
                    .get(time)
                    .and_then(|v| v.parse().ok())
                    .with_context(|| format!("bad run time in {l:?}"))?;
                out.push(Sample::new(name, t, "s", Better::Lower));
            }
            st => bad.push(format!("{name} ({})", st.unwrap_or("?"))),
        }
    }
    if !bad.is_empty() {
        bail!("failed: {}", bad.join(", "));
    }
    Ok(out)
}

fn spec(log: &str) -> Outcome {
    let mut running = BTreeSet::new();
    let mut ok = BTreeSet::new();
    let mut build_failed = BTreeSet::new();
    let mut nothing_to_run = false;
    for l in log.lines() {
        let t = l.trim_start();
        if let Some(rest) = t.strip_prefix("Running ") {
            if let Some(name) = spec_name(rest) {
                running.insert(name);
            }
        } else if let Some(rest) = t.strip_prefix("Success: ") {
            for s in rest.split_whitespace() {
                if let Some((_, name)) = s.split_once('x') {
                    ok.insert(String::from(name));
                }
            }
        } else if let Some((_, list)) = t
            .strip_prefix("Build errors for ")
            .and_then(|r| r.split_once(": "))
        {
            // "500.perlbench_r(base; nobuild), 502.gcc_r(base; nobuild)" oppure "None"
            if list.trim() != "None" {
                for item in list.split(", ") {
                    let name = item.split('(').next().unwrap_or(item).trim();
                    build_failed.insert(String::from(name));
                }
            }
        } else if t.starts_with("NOTICE: Nothing to run!") {
            nothing_to_run = true;
        }
    }
    let mut failed = running.difference(&ok).cloned().collect::<Vec<_>>();
    failed.extend(build_failed.iter().map(|b| format!("{b} (build)")));
    if nothing_to_run {
        failed.push(String::from("(nothing to run)"));
    }
    if running.is_empty() && failed.is_empty() {
        failed.push(String::from("(no summary: runcpu did not run anything)"));
    }
    let total = running.union(&build_failed).count();
    let n_failed = running.difference(&ok).count() + build_failed.len();
    Outcome {
        counts: Some(Counts {
            total,
            failed: n_failed,
            skipped: 0,
        }),
        failed,
    }
}

fn spec_name(s: &str) -> Option<String> {
    let mut name = String::new();
    let mut n = 0;
    for c in s.chars() {
        if c.is_ascii_digit() {
            name.push(c);
            n += 1;
        } else {
            break;
        }
    }
    if n == 0 {
        return None;
    }
    for c in s[n..].chars() {
        if c.is_whitespace() {
            return Some(name);
        } else {
            name.push(c);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_outcomes() {
        let ok = "\
  Running 508.namd_r test base vanilla (1 copy) [2026-11-11 23:43:50]
  Running 519.lbm_r test base vanilla (1 copy) [2026-11-11 23:43:55]
Success: 1x508.namd_r 1x519.lbm_r
";
        let o = FailureParser::Spec.parse(ok);
        assert!(o.failed.is_empty());
        assert_eq!(
            o.counts,
            Some(Counts {
                total: 2,
                failed: 0,
                skipped: 0
            })
        );

        let one_failed = "\
  Running 508.namd_r test base func (1 copy) [2026-11-11 23:43:50]
  Running 519.lbm_r test base func (1 copy) [2026-11-11 23:43:55]
Success: 1x508.namd_r
";
        assert_eq!(FailureParser::Spec.parse(one_failed).failed, ["519.lbm_r"]);

        let not_built = "\
Build errors for intrate: 500.perlbench_r(base; nobuild), 502.gcc_r(base; nobuild)
NOTICE: Nothing to run!  No results will be generated.
";
        let o = FailureParser::Spec.parse(not_built);
        assert_eq!(
            o.failed,
            [
                "500.perlbench_r (build)",
                "502.gcc_r (build)",
                "(nothing to run)"
            ]
        );
    }

    #[test]
    fn catch2_summary_with_skipped() {
        let log = "test cases:    7848 |    7823 passed | 25 failed | 162 skipped\n";
        let o = FailureParser::Catch2.parse(log);
        assert_eq!(
            o.counts,
            Some(Counts {
                total: 7848,
                failed: 25,
                skipped: 162
            })
        );
    }

    #[test]
    fn sqlite_summary() {
        let ok = "0 errors out of 334087 tests in 00:35 on localhost.localdomain Linux 64-bit\n";
        let o = FailureParser::Sqlite.parse(ok);
        assert!(o.failed.is_empty());
        assert_eq!(
            o.counts,
            Some(Counts {
                total: 334087,
                failed: 0,
                skipped: 0
            })
        );

        let interrupted = "00:12 tcl(400/1210) ETC 00:20\n";
        assert_eq!(FailureParser::Sqlite.parse(interrupted).failed.len(), 1);
    }

    #[test]
    fn php_summary() {
        let log = "\
Number of tests : 100                80
Tests skipped   :    20 ( 20.0%) --------
Tests warned    :     0 (  0.0%) (  0.0%)
Tests failed    :     2 (  2.0%) (  2.5%)
Expected fail   :     1 (  1.0%) (  1.3%)
Tests passed    :    77 ( 77.0%) ( 96.3%)
=====================================================================
FAILED TEST SUMMARY
---------------------------------------------------------------------
Bug #1 (something) [Zend/tests/bug1.phpt]
Test foo() with [brackets] in the title [ext/standard/tests/foo.phpt]
=====================================================================
";
        let o = FailureParser::Php.parse(log);
        assert_eq!(
            o.failed,
            ["Zend/tests/bug1.phpt", "ext/standard/tests/foo.phpt"]
        );
        assert_eq!(
            o.counts,
            Some(Counts {
                total: 100,
                failed: 2,
                skipped: 20
            })
        );
    }

    #[test]
    fn tap_pg_regress() {
        let log = "\
# parallel group (2 tests):  boolean char
ok 1         - test_setup                                366 ms
not ok 2     + boolean                                    12 ms
ok 3         + char                                        8 ms
ok 4 - optional # SKIP no feature
not ok 5 - flaky # TODO known issue
";
        let o = FailureParser::Tap.parse(log);
        assert_eq!(o.failed, ["boolean"]);
        assert_eq!(
            o.counts,
            Some(Counts {
                total: 5,
                failed: 1,
                skipped: 1
            })
        );
    }

    #[test]
    fn pg_sections_and_prove() {
        let log = "\
# +++ tap check in src/bin/pgbench +++
t/001_pgbench_with_server.pl .. ok
t/002_pgbench_no_server.pl .... ok
Test Summary Report
-------------------
t/001_pgbench_with_server.pl (Wstat: 0 Tests: 469 Failed: 0)
  TODO passed:   4-6
Files=2, Tests=677,  3 wallclock secs
# +++ tap check in src/bin/psql +++
t/001_basic.pl ........... ok
t/010_tab_completion.pl .. skipped: readline is not supported by this build
t/020_cancel.pl .......... Dubious, test returned 1
Test Summary Report
-------------------
t/020_cancel.pl (Wstat: 256 (exited 1) Tests: 3 Failed: 1)
Files=3, Tests=133,  1 wallclock secs
";
        let o = FailureParser::Pg.parse(log);
        assert_eq!(o.failed, ["src/bin/psql/t/020_cancel.pl"]);
        assert_eq!(
            o.counts,
            Some(Counts {
                total: 5,
                failed: 1,
                skipped: 1
            })
        );
    }

    #[test]
    fn redis_colored() {
        let log = "\
[\x1b[0;32;49mok\x1b[0m]: BITCOUNT against wrong type (0 ms)
[\x1b[0;31;49merr\x1b[0m]: LPOS RANK in tests/unit/type/list.tcl
[\x1b[0;33;49mskip\x1b[0m]: Test requires jemalloc
";
        let o = FailureParser::Redis.parse(log);
        assert_eq!(o.failed, ["LPOS RANK in tests/unit/type/list.tcl"]);
        assert_eq!(
            o.counts,
            Some(Counts {
                total: 3,
                failed: 1,
                skipped: 1
            })
        );
    }

    #[test]
    fn wrk_ok() {
        let log = "Running 10s test\n  2 threads and 64 connections\nRequests/sec:  51234.50\nTransfer/sec: 41.2MB\n";
        assert_eq!(wrk(log).unwrap()[0].value, 51234.5);
        assert!(
            wrk("Socket errors: connect 0, read 3, write 0, timeout 0\nRequests/sec: 1\n").is_err()
        );
    }

    #[test]
    fn redis_ok() {
        let log = "\"test\",\"rps\",\"avg_latency_ms\",\"min_latency_ms\",\"p50_latency_ms\",\"p95_latency_ms\",\"p99_latency_ms\",\"max_latency_ms\"\n\"SET\",\"98231.43\",\"0.271\",\"0.080\",\"0.263\",\"0.375\",\"0.471\",\"1.231\"\n\"LRANGE_100 (first 100 elements)\",\"40000.00\",\"1.0\",\"0.5\",\"0.9\",\"1.2\",\"1.5\",\"3.0\"\n";
        let s = redis_benchmark(log).unwrap();
        assert_eq!(s.len(), 6);
        assert_eq!(s[0].metric, "SET.rps");
        assert_eq!(s[5].metric, "LRANGE_100.p99");
    }

    #[test]
    fn spec_selected() {
        let csv = "valid,0\n\n\"Full Results Table\"\n\nBenchmark,\"Base # Copies\",\"Est. Base Run Time\",\"Est. Base Rate\",\"Base Selected\",\"Base Status\",\"Peak # Copies\",\"Est. Peak Run Time\",\"Est. Peak Rate\",\"Peak Selected\",\"Peak Status\",Description\n508.namd_r,1,99.0,--,0,S,,,,,,\"ref iteration #1\"\n\n\"Selected Results Table\"\n\nBenchmark,\"Base # Copies\",\"Est. Base Run Time\",\"Est. Base Rate\",\"Base Selected\",\"Base Status\",\"Peak # Copies\",\"Est. Peak Run Time\",\"Est. Peak Rate\",\"Peak Selected\",\"Peak Status\",Description\n503.bwaves_r,,,,,NR,,,,,NR\n508.namd_r,1,1.473951,--,1,S,,,,,NR,\"SelectedIteration (base #1; peak NR)\"\n519.lbm_r,1,0.770007,--,1,S,,,,,NR,\"SelectedIteration (base #1; peak NR)\"\n\n\"SPECrate2017_fp_base\",--\n";
        let s = spec_csv(csv).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].metric.as_str(), s[0].value), ("508.namd_r", 1.473951));
        let ve = csv.replace(
            "519.lbm_r,1,0.770007,--,1,S",
            "519.lbm_r,1,0.770007,--,1,VE",
        );
        assert!(spec_csv(&ve).is_err());
    }

    #[test]
    fn pgbench_ok() {
        let log = "transaction type: <builtin: select only>\nscaling factor: 10\nnumber of clients: 8\nnumber of transactions actually processed: 1234567\nnumber of failed transactions: 0 (0.000%)\nlatency average = 0.194 ms\ninitial connection time = 9.871 ms\ntps = 41152.233041 (without initial connection time)\n";
        let s = pgbench(log).unwrap();
        assert_eq!((s[0].metric.as_str(), s[0].value), ("tps", 41152.233041));
        assert_eq!((s[1].metric.as_str(), s[1].value), ("latency", 0.194));
        let failed = log.replace(
            "failed transactions: 0 (0.000%)",
            "failed transactions: 3 (0.001%)",
        );
        assert!(pgbench(&failed).is_err());
    }
}
