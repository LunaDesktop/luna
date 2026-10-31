//! `luna-security` — daemon prywatności Luny + narzędzie CLI.
//!
//! ```text
//! luna-security                     # uruchamia daemon (także: luna-security daemon)
//! luna-security status [--json]     # aktywność i oczekujące zgody
//! luna-security watch               # status odświeżany co sekundę
//! luna-security pending             # tylko oczekujące prośby
//! luna-security allow <app> [cap]   # cap: mic|camera|network|all (domyślnie network)
//! luna-security deny <app> [cap]
//! luna-security reset <app>         # usuwa politykę aplikacji
//! luna-security log [n]             # ostatnie wpisy dziennika
//! luna-security scan                # jednorazowy skan (bez zapisu)
//! ```
//!
//! `allow`/`deny` zapisują `~/.config/luna/security.conf`; decyzje z modala
//! w powłoce trafiają do daemona przez `$XDG_RUNTIME_DIR/luna-security/decision`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::error::Error;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use luna::security::{
    self, Activity, AppMap, Capability, PipewireStream, Policy, Prompt, Snapshot, Status,
};

static RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" fn on_stop(_: libc::c_int) {
    RUNNING.store(false, Ordering::Relaxed);
}

fn main() -> Result<(), Box<dyn Error>> {
    libluna_i18n::set_locale(libluna_i18n::preferred_locale());
    libluna_i18n::load_catalogs_from_dirs();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("daemon") | Some("--daemon") => run_daemon(),
        Some("status") => cmd_status(args.iter().any(|a| a == "--json")),
        Some("watch") => cmd_watch(),
        Some("pending") => cmd_pending(),
        Some("allow") => cmd_policy(&args, true),
        Some("deny") => cmd_policy(&args, false),
        Some("reset") => cmd_reset(&args),
        Some("log") => cmd_log(&args),
        Some("scan") => cmd_scan(),
        Some("-h") | Some("--help") | Some("help") => {
            usage();
            Ok(())
        }
        Some("--version") | Some("-V") => {
            println!("luna-security {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(other) => {
            eprintln!("luna-security: nieznana komenda: {other}");
            usage();
            std::process::exit(2);
        }
    }
}

fn usage() {
    println!(
        "luna-security — monitor prywatności Luny\n\
         \n\
         użycie:\n\
           luna-security                      uruchom daemon\n\
           luna-security status [--json]      aktywność i oczekujące zgody\n\
           luna-security watch                status co sekundę\n\
           luna-security pending              tylko oczekujące prośby\n\
           luna-security allow <app> [cap]    cap: mic|camera|network|all (domyślnie network)\n\
           luna-security deny <app> [cap]\n\
           luna-security reset <app>          usuń politykę aplikacji\n\
           luna-security log [n]              ostatnie wpisy (domyślnie 40)\n\
           luna-security scan                 jednorazowy skan\n"
    );
}

fn cmd_status(json: bool) -> Result<(), Box<dyn Error>> {
    let snap = security::load_snapshot();
    if json {
        println!("{}", json_snapshot(&snap));
        return Ok(());
    }
    println!("Luna Security — status");
    if snap.activities.is_empty() {
        println!("  Aktywne: brak");
    } else {
        println!("  Aktywne:");
        for a in &snap.activities {
            let caps: Vec<&str> = a.caps.iter().map(|c| c.key()).collect();
            let pids: Vec<String> = a.pids.iter().map(|p| p.to_string()).collect();
            let remote = a
                .remote
                .as_deref()
                .map(|r| format!(" → {r}"))
                .unwrap_or_default();
            println!(
                "    {:<8} {} ({})  [{}]  pid {}{}",
                caps.join(","),
                a.name,
                a.app,
                status_label(a.status),
                pids.join(","),
                remote
            );
        }
    }
    if snap.prompts.is_empty() {
        println!("  Oczekujące zgody: brak");
    } else {
        println!("  Oczekujące zgody:");
        for p in &snap.prompts {
            let remote = p
                .remote
                .as_deref()
                .map(|r| format!(" → {r}"))
                .unwrap_or_default();
            println!(
                "    {:<24} {:<10} {}{}",
                p.name,
                p.capability.key(),
                p.app,
                remote
            );
        }
    }
    if let Some(path) = Policy::path() {
        println!("  Polityka: {}", path.display());
    }
    if let Some(path) = security::log_path() {
        println!("  Dziennik: {}", path.display());
    }
    Ok(())
}

fn status_label(status: Status) -> &'static str {
    match status {
        Status::Allowed => "dozwolone",
        Status::Ask => "czeka na zgodę",
        Status::Denied => "odmowa",
    }
}

fn cmd_pending() -> Result<(), Box<dyn Error>> {
    let snap = security::load_snapshot();
    if snap.prompts.is_empty() {
        println!("Brak oczekujących zgód.");
    }
    for p in &snap.prompts {
        println!(
            "{}  {}  {}  {}",
            p.id,
            p.name,
            p.capability.key(),
            p.remote.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

fn cmd_watch() -> Result<(), Box<dyn Error>> {
    loop {
        print!("\x1b[2J\x1b[H");
        let _ = cmd_status(false);
        let _ = std::io::stdout().flush();
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn cmd_policy(args: &[String], allow: bool) -> Result<(), Box<dyn Error>> {
    let Some(query) = args.get(1) else {
        eprintln!(
            "użycie: luna-security {} <app> [mic|camera|network|all]",
            args[0]
        );
        std::process::exit(2);
    };
    let (app, name) = AppMap::load()
        .resolve(query)
        .unwrap_or_else(|| (query.to_ascii_lowercase(), query.to_owned()));
    let caps = requested_caps(args.get(2).map(String::as_str));
    let mut policy = Policy::load();
    for cap in &caps {
        security::apply_decision(&mut policy, &app, *cap, allow);
    }
    policy.save()?;
    println!(
        "{}: {} ({}) — {}",
        if allow { "Zezwolono" } else { "Zablokowano" },
        name,
        app,
        caps.iter().map(|c| c.key()).collect::<Vec<_>>().join(",")
    );
    // Daemon wyczyści pasujące prośby przy najbliższym ticku.
    for cap in &caps {
        let _ = security::decide(&format!("{app}:{}", cap.key()), allow);
    }
    Ok(())
}

fn requested_caps(arg: Option<&str>) -> BTreeSet<Capability> {
    match arg {
        Some("all") | Some("*") => Capability::ALL.into_iter().collect(),
        Some(value) => Capability::from_key(value)
            .map(|c| [c].into_iter().collect())
            .unwrap_or_else(|| Capability::ALL.into_iter().collect()),
        None => [Capability::Network].into_iter().collect(),
    }
}

fn cmd_reset(args: &[String]) -> Result<(), Box<dyn Error>> {
    let Some(query) = args.get(1) else {
        eprintln!("użycie: luna-security reset <app>");
        std::process::exit(2);
    };
    let (app, name) = AppMap::load()
        .resolve(query)
        .unwrap_or_else(|| (query.to_ascii_lowercase(), query.to_owned()));
    let mut policy = Policy::load();
    policy.clear(&app);
    policy.save()?;
    println!("Usunięto politykę dla {name} ({app}).");
    Ok(())
}

fn cmd_log(args: &[String]) -> Result<(), Box<dyn Error>> {
    let n = args
        .get(1)
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(40);
    for line in security::tail_log(n) {
        println!("{line}");
    }
    Ok(())
}

fn cmd_scan() -> Result<(), Box<dyn Error>> {
    let obs: Vec<_> = security::scan_all()
        .into_iter()
        .filter(|o| !o.caps.is_empty())
        .collect();
    if obs.is_empty() {
        println!("Brak aktywnej prywatności (mikrofon/kamera/sieć).");
        return Ok(());
    }
    for o in obs {
        let caps: Vec<&str> = o.caps.iter().map(|c| c.key()).collect();
        let pids: Vec<String> = o.pids.iter().map(|p| p.to_string()).collect();
        println!(
            "{:<8} {:<28} pid {}{}",
            caps.join(","),
            o.name,
            pids.join(","),
            o.remote
                .as_deref()
                .map(|r| format!(" → {r}"))
                .unwrap_or_default()
        );
    }
    Ok(())
}

// ── JSON (status --json) ────────────────────────────────────────────────────

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn json_snapshot(snap: &Snapshot) -> String {
    let mut out = String::from("{\"activities\":[");
    for (i, a) in snap.activities.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let caps: Vec<String> = a.caps.iter().map(|c| format!("\"{}\"", c.key())).collect();
        let pids: Vec<String> = a.pids.iter().map(|p| p.to_string()).collect();
        out.push_str(&format!(
            "{{\"app\":\"{}\",\"name\":\"{}\",\"caps\":[{}],\"pids\":[{}],\"remote\":{},\"status\":\"{}\",\"since\":{}}}",
            json_escape(&a.app),
            json_escape(&a.name),
            caps.join(","),
            pids.join(","),
            a.remote
                .as_deref()
                .map(|r| format!("\"{}\"", json_escape(r)))
                .unwrap_or_else(|| "null".into()),
            a.status.key(),
            a.since
        ));
    }
    out.push_str("],\"prompts\":[");
    for (i, p) in snap.prompts.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"id\":\"{}\",\"app\":\"{}\",\"name\":\"{}\",\"capability\":\"{}\",\"remote\":{},\"created\":{}}}",
            json_escape(&p.id),
            json_escape(&p.app),
            json_escape(&p.name),
            p.capability.key(),
            p.remote
                .as_deref()
                .map(|r| format!("\"{}\"", json_escape(r)))
                .unwrap_or_else(|| "null".into()),
            p.created
        ));
    }
    out.push_str("]}");
    out
}

// ── Daemon ──────────────────────────────────────────────────────────────────

struct Daemon {
    map: AppMap,
    policy: Policy,
    activities: BTreeMap<String, Activity>,
    prompts: BTreeMap<String, Prompt>,
    last_seen: HashMap<String, u64>,
    last_status: HashMap<(String, Capability), Status>,
    pipewire: Vec<PipewireStream>,
    ticks: u64,
    last_state: String,
}

impl Daemon {
    fn new() -> Self {
        Self {
            map: AppMap::load(),
            policy: Policy::load(),
            activities: BTreeMap::new(),
            prompts: BTreeMap::new(),
            last_seen: HashMap::new(),
            last_status: HashMap::new(),
            pipewire: Vec::new(),
            ticks: 0,
            last_state: String::new(),
        }
    }

    fn tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        self.apply_decisions();
        if self.ticks == 1 || self.ticks % 60 == 0 {
            self.map = AppMap::load();
        }
        if self.ticks == 1 || self.ticks % 3 == 0 {
            self.pipewire = security::pipewire_streams();
        }
        let now = security::now();
        let obs = security::scan_with(&self.map, &self.pipewire);
        for o in obs {
            if o.caps.is_empty() {
                continue;
            }
            self.last_seen.insert(o.app.clone(), now);
            let entry = self
                .activities
                .entry(o.app.clone())
                .or_insert_with(|| Activity {
                    app: o.app.clone(),
                    name: o.name.clone(),
                    caps: BTreeSet::new(),
                    pids: BTreeSet::new(),
                    remote: None,
                    status: Status::Ask,
                    since: now,
                });
            if !o.name.is_empty() {
                entry.name = o.name;
            }
            entry.caps.extend(o.caps.iter().copied());
            entry.pids = o.pids;
            if let Some(remote) = o.remote {
                entry.remote = Some(remote);
            }
        }

        let active: Vec<String> = self.activities.keys().cloned().collect();
        for app in active {
            let idle = now.saturating_sub(*self.last_seen.get(&app).unwrap_or(&now));
            if idle > 6 {
                self.activities.remove(&app);
                self.prompts.retain(|_, p| p.app != app);
                self.last_status.retain(|(a, _), _| a != &app);
                continue;
            }
            let caps = self.activities[&app].caps.clone();
            let mut overall = Status::Allowed;
            for cap in caps {
                let cap_status = if self.policy.denies(&app, cap) {
                    Status::Denied
                } else if self.policy.allows(&app, cap)
                    || security::is_trusted_app(&app)
                    || security::is_background_tool(&app)
                {
                    Status::Allowed
                } else {
                    Status::Ask
                };
                if cap_status == Status::Denied
                    || (cap_status == Status::Ask && overall != Status::Denied)
                {
                    overall = cap_status;
                }
                if security::needs_consent(&self.policy, &app, cap) {
                    let id = format!("{app}:{}", cap.key());
                    let remote = self.activities[&app]
                        .remote
                        .clone()
                        .filter(|_| cap == Capability::Network);
                    let name = self.activities[&app].name.clone();
                    let fresh = !self.prompts.contains_key(&id);
                    self.prompts.entry(id.clone()).or_insert_with(|| Prompt {
                        id,
                        app: app.clone(),
                        name: name.clone(),
                        capability: cap,
                        remote,
                        created: now,
                    });
                    if fresh {
                        security::log_line(&format!(
                            "{now} prompt app={app} name={name:?} cap={}",
                            cap.key()
                        ));
                    }
                } else {
                    self.prompts.remove(&format!("{app}:{}", cap.key()));
                }
                let key = (app.clone(), cap);
                if self.last_status.get(&key) != Some(&cap_status) {
                    self.last_status.insert(key, cap_status);
                    security::log_line(&format!(
                        "{now} activity app={app} cap={} status={}",
                        cap.key(),
                        cap_status.key()
                    ));
                }
            }
            if let Some(entry) = self.activities.get_mut(&app) {
                entry.status = overall;
            }
        }

        // Prośby bez aktywnej aplikacji (np. po restarcie) nie wiszą w nieskończoność.
        self.prompts
            .retain(|_, p| self.activities.contains_key(&p.app));

        let snap = Snapshot {
            activities: self.activities.values().cloned().collect(),
            prompts: self.prompts.values().cloned().collect(),
        };
        let text = snap.to_text();
        if text != self.last_state {
            if let Some(path) = security::state_path() {
                if let Err(err) = libluna_core::config::save_atomic(&path, &text) {
                    eprintln!("luna-security: nie mogę zapisać stanu: {err}");
                }
            }
            self.last_state = text;
        }
    }

    fn apply_decisions(&mut self) {
        for decision in security::read_decisions() {
            let target = self
                .prompts
                .get(&decision.prompt)
                .map(|p| (p.app.clone(), p.capability))
                .or_else(|| {
                    let (app, cap) = decision.prompt.rsplit_once(':')?;
                    Some((app.to_owned(), Capability::from_key(cap)?))
                });
            let Some((app, cap)) = target else {
                continue;
            };
            security::apply_decision(&mut self.policy, &app, cap, decision.allow);
            self.prompts.remove(&decision.prompt);
            let _ = self.policy.save();
        }
    }
}

fn run_daemon() -> Result<(), Box<dyn Error>> {
    if let Some(path) = security::ready_path() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(pid) = text.trim().parse::<u32>() {
                if unsafe { libc::kill(pid as i32, 0) } == 0 {
                    eprintln!("luna-security: daemon już działa (pid {pid})");
                    return Ok(());
                }
            }
        }
    }
    unsafe {
        libc::signal(libc::SIGTERM, on_stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_stop as *const () as libc::sighandler_t);
    }
    // Decyzje z poprzedniego uruchomienia są nieaktualne — polityka jest źródłem prawdy.
    if let Some(path) = security::decision_path() {
        let _ = std::fs::remove_file(path);
    }
    let pid = std::process::id();
    if let Some(path) = security::ready_path() {
        let _ = std::fs::write(&path, format!("{pid}\n"));
    }
    security::log_line(&format!("{} daemon start pid={pid}", security::now()));
    let mut daemon = Daemon::new();
    while RUNNING.load(Ordering::Relaxed) {
        daemon.tick();
        std::thread::sleep(Duration::from_secs(1));
    }
    security::log_line(&format!("{} daemon stop pid={pid}", security::now()));
    if let Some(path) = security::ready_path() {
        let _ = std::fs::remove_file(path);
    }
    // Bez daemona nie ma czym potwierdzać aktywności — nie zostawiamy starych ikon.
    if let Some(path) = security::state_path() {
        let _ = std::fs::remove_file(path);
    }
    if let Some(path) = security::decision_path() {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}
