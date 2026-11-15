use crate::common::{
    NonEmptySet, Vars, clone, command, expand, repo_dir, run_logged, run_quiet, spawn_logged, vars,
};
use crate::conf::BuildConf;
use crate::parsers::{Better, Sample};
use crate::program::{Case, ClientServer, Process, Program, Ready, Single, StopSignal};
use crate::toolchain::Toolchain;
use crate::{RunMode, doctor};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Tempo massimo per l'avvio del server (fino a `ready`) e per il suo arresto.
const START_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Una configurazione da misurare: i placeholder espansi e la cartella dei suoi risultati.
struct Target {
    conf: BuildConf,
    vars: Vars,
    /// cwd di default dei processi: la cartella di build, come per i test
    build: PathBuf,
    out: PathBuf,
}

/// Una riga di results.csv.
struct Row {
    case: usize,
    conf: BuildConf,
    sample: Sample,
}

pub fn run(
    tc: &Toolchain,
    program: &str,
    builds: &NonEmptySet<BuildConf>,
    only: Option<&NonEmptySet<String>>,
    jobs: usize,
    mode: RunMode,
) -> Result<()> {
    // vanilla è il riferimento dell'overhead: c'è sempre, ed è la prima
    let mut aug = NonEmptySet::single(BuildConf::Vanilla);
    builds.iter().for_each(|b| aug.push(*b));
    if mode != RunMode::DryRun {
        doctor::require(tc, &aug)?;
    }
    let prog = Program::load(&tc.programs, program)?;
    let Some(bench) = &prog.bench else {
        bail!("{}: no [bench] section in {}.toml", prog.name, prog.name);
    };
    let cases = select(bench.cases.list(), only)?;
    prepare_repos(tc, &prog, jobs, mode)?;

    let out = if mode == RunMode::DryRun {
        tc.results.join("bench").join(&prog.name).join("dry-run")
    } else {
        results_dir(&tc.results, &prog.name)?
    };
    let mut targets = Vec::new();
    for conf in aug.iter().copied() {
        let dirs = tc.layout(&prog, conf);
        if !dirs.prefix.is_dir() || !dirs.build.is_dir() {
            bail!("{conf}: not built (run `btp build {} {conf}`)", prog.name);
        }
        let conf_out = out.join(conf.name());
        let mut vars = vars(tc, &prog, conf, &dirs, jobs);
        vars.insert("out".into(), conf_out.display().to_string());
        targets.push(Target {
            conf,
            vars,
            build: dirs.build.clone(),
            out: conf_out,
        });
    }

    if mode == RunMode::DryRun {
        return dry_run(&prog, &cases, &targets);
    }

    let mut csv = File::create(out.join("results.csv"))?;
    writeln!(csv, "conf,case,metric,rep,value,unit,better")?;
    let mut rows: Vec<Row> = Vec::new();
    // (caso, configurazione) che hanno già fallito: non si ripetono
    let mut broken: BTreeSet<(usize, BuildConf)> = BTreeSet::new();

    for (ci, case) in cases.iter().enumerate() {
        let (warmup, repeat) = case.runs(bench);
        println!(
            "== {} {} (warmup {warmup}, repeat {repeat})",
            prog.name,
            case.name()
        );
        // Single: il riscaldamento sono giri completi scartati, alternati come le misure.
        // ClientServer: il server riparte a ogni misura, quindi il riscaldamento avviene
        // dentro ogni misura, dopo `ready`.
        let rounds = match case {
            Case::Single(_) => warmup + repeat,
            Case::ClientServer(_) => repeat,
        };
        for round in 0..rounds {
            let (rep, discard) = match case {
                Case::Single(_) if round < warmup => (round + 1, true),
                Case::Single(_) => (round - warmup + 1, false),
                Case::ClientServer(_) => (round + 1, false),
            };
            // configurazioni alternate, e a ogni giro si parte da una diversa: nessuna è
            // sempre la prima dopo un caso precedente (cache, frequenza della CPU)
            for k in 0..targets.len() {
                let t = &targets[(k + round as usize) % targets.len()];
                if broken.contains(&(ci, t.conf)) {
                    continue;
                }
                let dir = t.out.join(case.name());
                fs::create_dir_all(&dir)
                    .with_context(|| format!("cannot create {}", dir.display()))?;
                let tag = if discard {
                    format!("warmup{rep}")
                } else {
                    rep.to_string()
                };
                let t0 = Instant::now();
                let res = match case {
                    Case::Single(c) => measure_single(c, t, &dir, &tag, mode),
                    Case::ClientServer(c) => measure_cs(c, t, &dir, &tag, warmup, mode),
                };
                let secs = t0.elapsed().as_secs_f64();
                match res {
                    Err(e) => {
                        println!("  {} {tag}: FAILED ({secs:.1} s): {e:#}", t.conf);
                        broken.insert((ci, t.conf));
                    }
                    Ok(_) if discard => println!("  {} {tag}: discarded ({secs:.1} s)", t.conf),
                    Ok(samples) => {
                        println!("  {} {tag}: {} ({secs:.1} s)", t.conf, brief(&samples));
                        for s in samples {
                            writeln!(
                                csv,
                                "{},{},{},{rep},{},{},{}",
                                t.conf,
                                case.name(),
                                s.metric,
                                s.value,
                                s.unit,
                                s.better.as_str()
                            )?;
                            rows.push(Row {
                                case: ci,
                                conf: t.conf,
                                sample: s,
                            });
                        }
                        // una misura alla volta su disco: un'interruzione non perde le precedenti
                        csv.flush()?;
                    }
                }
            }
        }
    }

    summary(&cases, &rows, &broken, &out)?;
    println!("\nresults in {}", out.display());
    Ok(())
}

/// I casi da eseguire, nell'ordine del toml; `only` (opzione -c) ne sceglie alcuni per nome.
fn select<'a>(
    all: &'a NonEmptySet<Case>,
    only: Option<&NonEmptySet<String>>,
) -> Result<Vec<&'a Case>> {
    let Some(only) = only else {
        return Ok(all.iter().collect());
    };
    for name in only {
        if !all.iter().any(|c| c.name() == name) {
            let known: Vec<&str> = all.iter().map(Case::name).collect();
            bail!(
                "unknown bench case \"{name}\" (known: {})",
                known.join(", ")
            );
        }
    }
    Ok(all
        .iter()
        .filter(|c| only.iter().any(|n| n == c.name()))
        .collect())
}

/// Un caso single: il comando strumentato, cronometrato da btp.
fn measure_single(
    c: &Single,
    t: &Target,
    dir: &Path,
    tag: &str,
    mode: RunMode,
) -> Result<Vec<Sample>> {
    let mut cmd = spawnable(&c.process(), &t.vars, &t.build)?;
    let log = dir.join(format!("{tag}.log"));
    let t0 = Instant::now();
    let status = spawn_logged(&mut cmd, &log, mode)?;
    let elapsed = t0.elapsed();
    if !status.success() {
        bail!("command failed ({status}), log: {}", log.display());
    }
    c.metric.parse(&read_log(&log)?, elapsed)
}

/// Un caso client-server: avvio del server, attesa di `ready`, setup, riscaldamento, misura,
/// arresto. La misura vale solo se il server è rimasto vivo fino all'arresto e si è fermato
/// in modo ordinato.
fn measure_cs(
    c: &ClientServer,
    t: &Target,
    dir: &Path,
    tag: &str,
    warmup: u32,
    mode: RunMode,
) -> Result<Vec<Sample>> {
    let server_log = dir.join(format!("{tag}-server.log"));
    let mut server = Server::start(&c.server, t, &server_log, &c.ready, mode)?;
    let ctx = |what: &str| format!("{what}, server log: {}", server_log.display());

    for (i, step) in c.setup.iter().enumerate() {
        let mut cmd = command(step, &t.vars, &t.build, &[]).with_context(|| ctx("setup"))?;
        run_logged(&mut cmd, &dir.join(format!("{tag}-setup{i}.log")), mode)?;
        server.alive().with_context(|| ctx("during setup"))?;
    }
    for w in 1..=warmup {
        let log = dir.join(format!("{tag}-warmup{w}.log"));
        run_client(&c.client, t, &log, mode)?;
        server.alive().with_context(|| ctx("during warmup"))?;
    }
    let log = dir.join(format!("{tag}-client.log"));
    let elapsed = run_client(&c.client, t, &log, mode)?;
    // prima di leggere il risultato: se il server è morto durante la misura, i numeri del
    // client (errori di connessione, risposte parziali) non descrivono il server
    server.stop(c.stop).with_context(|| ctx("stop"))?;
    c.metric.parse(&read_log(&log)?, elapsed)
}

/// Esegue il client e ne restituisce la durata.
fn run_client(p: &Process, t: &Target, log: &Path, mode: RunMode) -> Result<Duration> {
    let mut cmd = spawnable(p, &t.vars, &t.build)?;
    let t0 = Instant::now();
    let status = spawn_logged(&mut cmd, log, mode)?;
    let elapsed = t0.elapsed();
    if !status.success() {
        bail!("client failed ({status}), log: {}", log.display());
    }
    Ok(elapsed)
}

/// Il server di una misura. Se la misura fallisce a metà, `Drop` lo uccide: nessun server
/// resta in ascolto sulla porta della misura successiva.
struct Server {
    child: Child,
}

impl Server {
    fn start(p: &Process, t: &Target, log: &Path, ready: &Ready, mode: RunMode) -> Result<Server> {
        if let Ready::Tcp(addr) = ready {
            // con la porta già occupata, `ready` riuscirebbe subito, ma verso un altro processo
            if TcpStream::connect_timeout(addr, Duration::from_millis(200)).is_ok() {
                bail!("{addr} is already in use: is another server still running?");
            }
        }
        let mut cmd = spawnable(p, &t.vars, &t.build)?;
        if mode == RunMode::Verbose {
            println!("$ {cmd:?}");
        }
        let f = File::create(log).with_context(|| format!("cannot create {}", log.display()))?;
        cmd.stdin(Stdio::null()).stdout(f.try_clone()?).stderr(f);
        let child = cmd.spawn().with_context(|| format!("cannot run {cmd:?}"))?;
        let mut s = Server { child };
        s.wait_ready(ready)
            .with_context(|| format!("server log: {}", log.display()))?;
        Ok(s)
    }

    fn wait_ready(&mut self, ready: &Ready) -> Result<()> {
        match ready {
            Ready::Delay(secs) => {
                sleep(Duration::from_secs_f64(*secs));
                self.alive()
            }
            Ready::Tcp(addr) => self.wait_port(addr),
        }
    }

    fn wait_port(&mut self, addr: &SocketAddr) -> Result<()> {
        let t0 = Instant::now();
        loop {
            self.alive()?;
            if TcpStream::connect_timeout(addr, Duration::from_millis(200)).is_ok() {
                return Ok(());
            }
            if t0.elapsed() > START_TIMEOUT {
                bail!("server not listening on {addr} after {START_TIMEOUT:?}");
            }
            sleep(Duration::from_millis(50));
        }
    }

    fn alive(&mut self) -> Result<()> {
        match self.child.try_wait()? {
            None => Ok(()),
            Some(status) => bail!("server exited unexpectedly ({status})"),
        }
    }

    /// Arresto ordinato con `sig`. Vale se il server esce con successo o per quel segnale;
    /// un crash durante l'arresto (es. SIGSEGV) è un errore della misura.
    fn stop(mut self, sig: StopSignal) -> Result<()> {
        self.alive()?;
        let signo = sig.number();
        // SAFETY: kill non tocca memoria; il pid è di un figlio non ancora raccolto (try_wait
        // sopra ha restituito None), quindi non può essere stato riassegnato
        if unsafe { libc::kill(self.child.id() as libc::pid_t, signo) } != 0 {
            return Err(std::io::Error::last_os_error()).context("kill");
        }
        let t0 = Instant::now();
        while t0.elapsed() < STOP_TIMEOUT {
            if let Some(status) = self.child.try_wait()? {
                if status.success() || status.signal() == Some(signo) {
                    return Ok(());
                }
                bail!("server stopped badly ({status})");
            }
            sleep(Duration::from_millis(20));
        }
        bail!(
            "server still running {STOP_TIMEOUT:?} after SIG{}, killed",
            sig.name()
        )
        // Drop uccide il processo
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Il comando di un processo di un benchmark, con i placeholder espansi e, se `cpus` è
/// presente, vincolato a quelle CPU con taskset (che poi esegue il programma con exec: il pid
/// resta quello del programma, e i segnali arrivano a lui).
fn spawnable(p: &Process, vars: &Vars, default_cwd: &Path) -> Result<Command> {
    let cwd = match &p.cwd {
        Some(d) => PathBuf::from(expand(d, vars)?),
        None => default_cwd.to_path_buf(),
    };
    let env = p
        .env
        .iter()
        .map(|(k, v)| Ok((k.clone(), expand(v, vars)?)))
        .collect::<Result<Vec<_>>>()?;
    let argv: Vec<String> = match &p.cpus {
        Some(cpus) => ["taskset", "-c", cpus]
            .into_iter()
            .map(String::from)
            .chain(p.cmd.iter().cloned())
            .collect(),
        None => p.cmd.clone(),
    };
    command(&argv, vars, &cwd, &env)
}

fn read_log(log: &Path) -> Result<String> {
    let bytes = fs::read(log).with_context(|| format!("cannot read {}", log.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Il primo valore di una misura, per la riga a schermo.
fn brief(samples: &[Sample]) -> String {
    match samples {
        [] => "no values".into(),
        [s] => format!("{} = {:.3} {}", s.metric, s.value, s.unit),
        [s, rest @ ..] => format!(
            "{} = {:.3} {} (+{} values)",
            s.metric,
            s.value,
            s.unit,
            rest.len()
        ),
    }
}

/// Clona le repos e compila quelle con `build`, una volta per versione: il file `.btp-built`
/// ricorda il ref compilato. I client (es. wrk) sono gli stessi per tutte le configurazioni.
fn prepare_repos(tc: &Toolchain, prog: &Program, jobs: usize, mode: RunMode) -> Result<()> {
    for (name, repo) in &prog.repos {
        let dir = repo_dir(&tc.targets, &prog.name, name);
        for mut c in clone(&repo.git, &repo.git_ref, &dir) {
            if mode == RunMode::DryRun {
                println!("{c:?}");
            } else {
                run_quiet(&mut c, mode)?;
            }
        }
        if repo.build.is_empty() {
            continue;
        }
        let stamp = dir.join(".btp-built");
        if fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == repo.git_ref) {
            continue;
        }
        let vars = Vars::from([
            ("jobs".into(), jobs.to_string()),
            ("src".into(), dir.display().to_string()),
        ]);
        if mode != RunMode::DryRun {
            println!("== repos.{name}: build");
        }
        for (i, step) in repo.build.iter().enumerate() {
            let mut c = command(step, &vars, &dir, &[])
                .with_context(|| format!("{}: repos.{name}.build {i}", prog.name))?;
            if mode == RunMode::DryRun {
                println!("{c:?}");
            } else {
                run_logged(&mut c, &dir.join(format!(".btp-build-{i}.log")), mode)?;
            }
        }
        if mode != RunMode::DryRun {
            fs::write(&stamp, &repo.git_ref)?;
        }
    }
    Ok(())
}

fn dry_run(prog: &Program, cases: &[&Case], targets: &[Target]) -> Result<()> {
    for case in cases {
        for t in targets {
            println!("# {} {} {}", prog.name, t.conf, case.name());
            match case {
                Case::Single(c) => println!("{:?}", spawnable(&c.process(), &t.vars, &t.build)?),
                Case::ClientServer(c) => {
                    println!("server: {:?}", spawnable(&c.server, &t.vars, &t.build)?);
                    println!("ready:  {:?}", c.ready);
                    for step in &c.setup {
                        println!("setup:  {:?}", command(step, &t.vars, &t.build, &[])?);
                    }
                    println!("client: {:?}", spawnable(&c.client, &t.vars, &t.build)?);
                    println!("stop:   SIG{}", c.stop.name());
                }
            }
        }
    }
    Ok(())
}

/// Mediana per (caso, metrica, configurazione) e overhead rispetto a vanilla, a schermo e in
/// summary.tsv. Overhead positivo = peggio di vanilla: per i valori "lower" è l'aumento
/// (m/v − 1), per gli "higher" il calo (1 − m/v).
fn summary(
    cases: &[&Case],
    rows: &[Row],
    broken: &BTreeSet<(usize, BuildConf)>,
    out: &Path,
) -> Result<()> {
    /// (unità, verso, valori per configurazione) di una coppia (caso, metrica)
    type Group<'a> = (&'a str, Better, BTreeMap<BuildConf, Vec<f64>>);
    let mut groups: BTreeMap<(usize, &str), Group> = BTreeMap::new();
    for r in rows {
        let g = groups.entry((r.case, r.sample.metric.as_str())).or_insert((
            r.sample.unit,
            r.sample.better,
            BTreeMap::new(),
        ));
        g.2.entry(r.conf).or_default().push(r.sample.value);
    }

    let mut tsv = File::create(out.join("summary.tsv"))?;
    writeln!(
        tsv,
        "case\tmetric\tunit\tbetter\tconf\tn\tmedian\tmin\tmax\toverhead"
    )?;
    println!("\n== summary (median; overhead vs vanilla, positive = worse)");
    for ((ci, metric), (unit, better, per_conf)) in &mut groups {
        let name = cases[*ci].name();
        let reference = per_conf.get_mut(&BuildConf::Vanilla).map(|v| median(v));
        println!("   {name} {metric} [{unit}, {} is better]", better.as_str());
        for (conf, values) in per_conf.iter_mut() {
            let m = median(values);
            let (min, max) = (values[0], values[values.len() - 1]);
            let overhead = reference.filter(|v| *v != 0.0).map(|v| match better {
                Better::Lower => m / v - 1.0,
                Better::Higher => 1.0 - m / v,
            });
            let shown = match overhead {
                Some(o) if *conf != BuildConf::Vanilla => format!("{:+.2}%", o * 100.0),
                _ => String::new(),
            };
            println!(
                "     {:<8} {m:>14.3}  (n={}, {min:.3}..{max:.3})  {shown}",
                conf.name(),
                values.len()
            );
            writeln!(
                tsv,
                "{name}\t{metric}\t{unit}\t{}\t{conf}\t{}\t{m}\t{min}\t{max}\t{}",
                better.as_str(),
                values.len(),
                overhead.map(|o| o.to_string()).unwrap_or_default()
            )?;
        }
    }
    if !broken.is_empty() {
        println!("\n== failed (case not measured further in that configuration)");
        for (ci, conf) in broken {
            println!("   {} {conf}", cases[*ci].name());
        }
    }
    Ok(())
}

/// Ordina `v` (min e max diventano il primo e l'ultimo) e ne restituisce la mediana.
fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Crea results/bench/<programma>/<data-ora>/.
fn results_dir(results: &Path, program: &str) -> Result<PathBuf> {
    let out = Command::new("date").arg("+%Y%m%d-%H%M%S").output()?;
    let stamp = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let dir = results.join("bench").join(program).join(stamp);
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    Ok(dir)
}
