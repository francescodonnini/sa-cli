use std::cmp::PartialEq;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureParser {
    Ctest,
    /// Solo codice d'uscita
    #[default]
    ExitCode,
    /// `make check` di PostgreSQL: pg_regress e prove, divisi in sezioni per cartella
    Pg,
    /// Riepilogo di prove (nginx)
    Prove,
    Redis,
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
            FailureParser::Ctest => return ctest(log),
            FailureParser::ExitCode => return Outcome::default(),
            FailureParser::Pg => (true, true, true),
            FailureParser::Prove => (false, true, false),
            FailureParser::Redis => return redis(log),
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
            counts: Some(Counts { total, failed: errors, skipped: 0 }),
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
    Outcome { counts: Some(c), failed }
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
    Outcome { counts: Some(c), failed }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_summary() {
        let ok = "0 errors out of 334087 tests in 00:35 on localhost.localdomain Linux 64-bit\n";
        let o = FailureParser::Sqlite.parse(ok);
        assert!(o.failed.is_empty());
        assert_eq!(o.counts, Some(Counts { total: 334087, failed: 0, skipped: 0 }));

        let interrupted = "00:12 tcl(400/1210) ETC 00:20\n";
        assert_eq!(FailureParser::Sqlite.parse(interrupted).failed.len(), 1);
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
        assert_eq!(o.counts, Some(Counts { total: 5, failed: 1, skipped: 1 }));
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
        assert_eq!(o.counts, Some(Counts { total: 5, failed: 1, skipped: 1 }));
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
        assert_eq!(o.counts, Some(Counts { total: 3, failed: 1, skipped: 1 }));
    }
}