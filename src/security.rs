//! `luna-security` — monitor prywatności i zgód aplikacji.
//!
//! Daemon (binarka `luna-security`) obserwuje:
//! - **mikrofon i kamerę** — strumienie PipeWire (`Stream/Input/Audio`,
//!   `Stream/Input/Video`) oraz bezpośrednie uchwyty `/dev/snd/pcm*c` i `/dev/video*`
//!   w `/proc/<pid>/fd`,
//! - **połączenia wychodzące** — publiczne adresy z `/proc/net/tcp{,6}` i `udp{,6}`
//!   powiązane z deskryptorami procesów,
//! - **tożsamość aplikacji** — pliki `.desktop` z [`crate::apps`] (po nazwie
//!   polecenia) albo `comm`/`exe`.
//!
//! Wynik jest zapisywany w prostym pliku stanu w `$XDG_RUNTIME_DIR/luna-security/`.
//! Powłoka (`luna-shell`) czyta ten plik i pokazuje ikony na panelu oraz modal
//! z prośbą o zgodę. CLI (`luna-security status/allow/deny/...`) czyta i pisze te
//! same pliki — nie trzeba D-Busa, a format jest łatwy do testowania.
//!
//! **Ważne:** to jest monitor i rejestr zgód, a nie firewall. Odmowa oznacza
//! „nie zgadzam się” (wpis w polityce + alert), ale nie blokuje pakietów.
//! Egzekwowanie ruchu (nftables/BPF) to osobny etap — patrz `docs/security.md`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use libluna_core::config::{config_file, save_atomic};

/// Uprawnienie monitorowane przez daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    Microphone,
    Camera,
    Network,
}

impl Capability {
    pub const ALL: [Capability; 3] = [
        Capability::Microphone,
        Capability::Camera,
        Capability::Network,
    ];

    /// Stabilna nazwa w plikach i CLI.
    pub fn key(self) -> &'static str {
        match self {
            Capability::Microphone => "mic",
            Capability::Camera => "camera",
            Capability::Network => "network",
        }
    }

    /// Nazwa do wyświetlenia (i klucz tłumaczenia w `libluna-i18n`).
    pub fn label(self) -> &'static str {
        match self {
            Capability::Microphone => "Microphone",
            Capability::Camera => "Camera",
            Capability::Network => "Network",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim().to_ascii_lowercase().as_str() {
            "mic" | "microphone" | "mikrofon" => Some(Capability::Microphone),
            "camera" | "kamera" | "cam" => Some(Capability::Camera),
            "network" | "net" | "siec" | "sieć" => Some(Capability::Network),
            _ => None,
        }
    }

    pub fn parse_list(value: &str) -> BTreeSet<Self> {
        value
            .split([',', ';', ' '])
            .filter_map(Self::from_key)
            .collect()
    }
}

/// Status aktywności aplikacji.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Allowed,
    Ask,
    Denied,
}

impl Status {
    pub fn key(self) -> &'static str {
        match self {
            Status::Allowed => "allow",
            Status::Ask => "ask",
            Status::Denied => "deny",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "allow" => Some(Status::Allowed),
            "ask" => Some(Status::Ask),
            "deny" => Some(Status::Denied),
            _ => None,
        }
    }
}

/// Jedna obserwowana aplikacja i jej aktywne uprawnienia.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    pub app: String,
    pub name: String,
    pub caps: BTreeSet<Capability>,
    pub pids: BTreeSet<u32>,
    /// Przykładowy publiczny adres (dla `Network`).
    pub remote: Option<String>,
    pub status: Status,
    pub since: u64,
}

/// Oczekująca prośba o zgodę (modal w powłoce / `luna-security pending`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub id: String,
    pub app: String,
    pub name: String,
    pub capability: Capability,
    pub remote: Option<String>,
    pub created: u64,
}

/// Migawka stanu czytana przez powłokę i CLI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub activities: Vec<Activity>,
    pub prompts: Vec<Prompt>,
}

impl Snapshot {
    pub fn active(&self, cap: Capability) -> bool {
        self.activities.iter().any(|a| a.caps.contains(&cap))
    }

    pub fn alert(&self) -> bool {
        !self.prompts.is_empty() || self.activities.iter().any(|a| a.status == Status::Denied)
    }

    pub fn prompt_for_cap(&self, cap: Capability) -> Option<&Prompt> {
        self.prompts.iter().find(|p| p.capability == cap)
    }
}

/// Katalog stanu runtime (`$XDG_RUNTIME_DIR/luna-security`).
pub fn runtime_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let uid = unsafe { libc::getuid() };
            std::env::temp_dir().join(format!("luna-security-{uid}"))
        });
    let dir = base.join("luna-security");
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Some(dir)
}

pub fn state_path() -> Option<PathBuf> {
    Some(runtime_dir()?.join("state"))
}

pub fn decision_path() -> Option<PathBuf> {
    Some(runtime_dir()?.join("decision"))
}

pub fn ready_path() -> Option<PathBuf> {
    Some(runtime_dir()?.join("ready"))
}

/// Znacznik modyfikacji pliku stanu (powłoka odświeża UI tylko po zmianie).
pub fn state_stamp() -> u64 {
    state_path()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn clean(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c == '\t' || c == '\n' || c == '\r' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

impl Snapshot {
    /// Serializacja TSV — jedna linia na aktywność/prośbę.
    pub fn to_text(&self) -> String {
        let mut out = String::from("# Luna Security state v1\n");
        for a in &self.activities {
            let caps: Vec<&str> = a.caps.iter().map(|c| c.key()).collect();
            let pids: Vec<String> = a.pids.iter().map(|p| p.to_string()).collect();
            out.push_str(&format!(
                "activity\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                clean(&a.app),
                clean(&a.name),
                caps.join(","),
                pids.join(","),
                clean(a.remote.as_deref().unwrap_or("")),
                a.since,
                a.status.key()
            ));
        }
        for p in &self.prompts {
            out.push_str(&format!(
                "prompt\t{}\t{}\t{}\t{}\t{}\t{}\n",
                clean(&p.id),
                clean(&p.app),
                clean(&p.name),
                p.capability.key(),
                clean(p.remote.as_deref().unwrap_or("")),
                p.created
            ));
        }
        out
    }

    pub fn parse(text: &str) -> Self {
        let mut snap = Snapshot::default();
        for line in text.lines() {
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = line.split('\t').collect();
            match f.first().copied() {
                Some("activity") if f.len() >= 8 => {
                    let caps = Capability::parse_list(f[3]);
                    let pids = f[4]
                        .split(',')
                        .filter_map(|p| p.trim().parse::<u32>().ok())
                        .collect();
                    let remote = (!f[5].is_empty()).then(|| f[5].to_owned());
                    if let Some(status) = Status::from_key(f[7]) {
                        snap.activities.push(Activity {
                            app: f[1].to_owned(),
                            name: f[2].to_owned(),
                            caps,
                            pids,
                            remote,
                            status,
                            since: f[6].parse().unwrap_or(0),
                        });
                    }
                }
                Some("prompt") if f.len() >= 7 => {
                    if let Some(cap) = Capability::from_key(f[4]) {
                        snap.prompts.push(Prompt {
                            id: f[1].to_owned(),
                            app: f[2].to_owned(),
                            name: f[3].to_owned(),
                            capability: cap,
                            remote: (!f[5].is_empty()).then(|| f[5].to_owned()),
                            created: f[6].parse().unwrap_or(0),
                        });
                    }
                }
                _ => {}
            }
        }
        snap
    }
}

/// Wczytuje migawkę; brak pliku = pusty stan.
pub fn load_snapshot() -> Snapshot {
    state_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|text| Snapshot::parse(&text))
        .unwrap_or_default()
}

/// Zapisuje decyzję dla modala/CLI. Daemon odczyta ją i zaktualizuje politykę.
pub fn decide(prompt_id: &str, allow: bool) -> io::Result<()> {
    let path =
        decision_path().ok_or_else(|| io::Error::other("brak katalogu runtime luna-security"))?;
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!(
        "{}\t{}\t1\n",
        clean(prompt_id),
        if allow { "allow" } else { "deny" }
    ));
    save_atomic(&path, &text)
}

/// Decyzja zapisana przez daemona/CLI do przetworzenia.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub prompt: String,
    pub allow: bool,
    pub remember: bool,
}

pub fn read_decisions() -> Vec<Decision> {
    let Some(path) = decision_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let _ = std::fs::remove_file(&path);
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() >= 2 {
            out.push(Decision {
                prompt: f[0].to_owned(),
                allow: f[1] == "allow",
                remember: f.get(2).copied() != Some("0"),
            });
        }
    }
    out
}

/// Trwała polityka: `~/.config/luna/security.conf`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    allow: BTreeMap<String, BTreeSet<Capability>>,
    deny: BTreeMap<String, BTreeSet<Capability>>,
}

impl Policy {
    pub fn path() -> Option<PathBuf> {
        config_file("security.conf")
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        std::fs::read_to_string(path)
            .map(|t| Self::parse(&t))
            .unwrap_or_default()
    }

    pub fn parse(text: &str) -> Self {
        let mut p = Policy::default();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            let Some((app, caps)) = value.split_once(':') else {
                continue;
            };
            let app = app.trim().to_owned();
            let caps = Capability::parse_list(caps);
            if app.is_empty() || caps.is_empty() {
                continue;
            }
            match key {
                "allow" => {
                    p.allow.entry(app).or_default().extend(caps);
                }
                "deny" => {
                    p.deny.entry(app).or_default().extend(caps);
                }
                _ => {}
            }
        }
        p
    }

    pub fn to_text(&self) -> String {
        let mut out = String::from(
            "# Luna Security — zgody aplikacji.\n\
             # allow=<aplikacja>:<mic,camera,network> albo deny=<aplikacja>:<...>\n",
        );
        for (app, caps) in &self.allow {
            out.push_str(&format!("allow={app}:{}\n", join_caps(caps)));
        }
        for (app, caps) in &self.deny {
            out.push_str(&format!("deny={app}:{}\n", join_caps(caps)));
        }
        out
    }

    pub fn save(&self) -> io::Result<()> {
        let path =
            Self::path().ok_or_else(|| io::Error::other("brak katalogu konfiguracji Luny"))?;
        save_atomic(&path, &self.to_text())
    }

    pub fn allows(&self, app: &str, cap: Capability) -> bool {
        self.allow.get(app).is_some_and(|c| c.contains(&cap))
    }

    pub fn denies(&self, app: &str, cap: Capability) -> bool {
        self.deny.get(app).is_some_and(|c| c.contains(&cap))
    }

    pub fn set_allow(&mut self, app: &str, cap: Capability) {
        if let Some(c) = self.deny.get_mut(app) {
            c.remove(&cap);
        }
        self.allow.entry(app.to_owned()).or_default().insert(cap);
    }

    pub fn set_deny(&mut self, app: &str, cap: Capability) {
        if let Some(c) = self.allow.get_mut(app) {
            c.remove(&cap);
        }
        self.deny.entry(app.to_owned()).or_default().insert(cap);
    }

    pub fn clear(&mut self, app: &str) {
        self.allow.remove(app);
        self.deny.remove(app);
    }

    pub fn entries(&self) -> Vec<(bool, String, BTreeSet<Capability>)> {
        let mut out: Vec<(bool, String, BTreeSet<Capability>)> = self
            .allow
            .iter()
            .map(|(a, c)| (true, a.clone(), c.clone()))
            .collect();
        out.extend(self.deny.iter().map(|(a, c)| (false, a.clone(), c.clone())));
        out
    }
}

fn join_caps(caps: &BTreeSet<Capability>) -> String {
    caps.iter().map(|c| c.key()).collect::<Vec<_>>().join(",")
}

/// Czy to składnik Luny (zaufany domyślnie — nie spamujemy go modalem).
pub fn is_trusted_app(app: &str) -> bool {
    let app = app.to_ascii_lowercase();
    app.starts_with("luna-")
        || app.starts_with("eu.luna.")
        || app.starts_with("eu.paganlinux.")
        || matches!(
            app.as_str(),
            "selene" | "luna" | "luna-shell" | "luna-session" | "luna-store"
        )
}

/// Czy adres jest poza komputerem (publiczny). Loopback, sieci prywatne,
/// link-local i multicast nie wywołują zgody.
pub fn is_public_remote(ip: IpAddr, port: u16) -> bool {
    if port == 0 {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                || o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || (o[0] == 169 && o[1] == 254)
                || (o[0] == 100 && (64..=127).contains(&o[1])))
        }
        IpAddr::V6(v6) => {
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80)
        }
    }
}

/// Parsuje `/proc/net/*` i zwraca mapę `inode → publiczny adres:port`.
pub fn public_remotes() -> HashMap<u64, String> {
    let mut out = HashMap::new();
    for file in [
        "/proc/net/tcp",
        "/proc/net/tcp6",
        "/proc/net/udp",
        "/proc/net/udp6",
    ] {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 10 {
                continue;
            }
            let Some((ip, port)) = parse_hex_endpoint(f[2]) else {
                continue;
            };
            if !is_public_remote(ip, port) {
                continue;
            }
            if let Ok(inode) = f[9].parse::<u64>() {
                out.entry(inode)
                    .or_insert_with(|| format!("{}:{port}", display_ip(ip)));
            }
        }
    }
    out
}

fn display_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V6(v6) => format!("[{v6}]"),
        IpAddr::V4(v4) => v4.to_string(),
    }
}

/// `0100007F:1F90` → `(127.0.0.1, 8080)`.
fn parse_hex_endpoint(value: &str) -> Option<(IpAddr, u16)> {
    let (addr, port) = value.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = match addr.len() {
        8 => {
            let raw = u32::from_str_radix(addr, 16).ok()?;
            let b = raw.to_le_bytes();
            IpAddr::from(b)
        }
        32 => {
            let mut bytes = [0u8; 16];
            for i in 0..4 {
                let group = u32::from_str_radix(&addr[i * 8..i * 8 + 8], 16).ok()?;
                bytes[i * 4..i * 4 + 4].copy_from_slice(&group.to_le_bytes());
            }
            IpAddr::from(bytes)
        }
        _ => return None,
    };
    Some((ip, port))
}

/// Wynik jednego skanu procesu.
#[derive(Debug, Clone, Default)]
pub struct Observation {
    pub app: String,
    pub name: String,
    pub pids: BTreeSet<u32>,
    pub caps: BTreeSet<Capability>,
    pub remote: Option<String>,
}

/// Strumień PipeWire zgłoszony przez `pw-dump`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipewireStream {
    pub pid: Option<u32>,
    pub name: Option<String>,
    pub capability: Capability,
}

/// Czyta aktywne strumienie wejściowe PipeWire (mikrofon/kamera).
///
/// `pw-dump` jest cięższy od `/proc`; daemon woła go rzadziej i cache'uje wynik.
pub fn pipewire_streams() -> Vec<PipewireStream> {
    let Ok(output) = std::process::Command::new("pw-dump").output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    let Some(nodes) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for node in nodes {
        if node.get("type").and_then(|v| v.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let props = node
            .get("info")
            .and_then(|v| v.get("props"))
            .or_else(|| node.get("props"));
        let Some(props) = props else {
            continue;
        };
        let media = props
            .get("media.class")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let capability = match media {
            "Stream/Input/Audio" => Capability::Microphone,
            "Stream/Input/Video" => Capability::Camera,
            _ => continue,
        };
        let pid = props
            .get("application.process.id")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .map(|v| v as u32);
        let name = props
            .get("application.name")
            .or_else(|| props.get("application.process.binary"))
            .or_else(|| props.get("node.name"))
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        out.push(PipewireStream {
            pid,
            name,
            capability,
        });
    }
    out
}

/// Mapa poleceń `.desktop` → `(klucz, nazwa)`.
pub struct AppMap {
    by_bin: HashMap<String, (String, String)>,
    by_name: HashMap<String, (String, String)>,
}

impl AppMap {
    pub fn load() -> Self {
        let mut by_bin = HashMap::new();
        let mut by_name = HashMap::new();
        for app in crate::apps::scan() {
            let key = app
                .desktop_path
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| app.name.clone());
            let entry = (key.clone(), app.name.clone());
            if let Some(bin) = exec_binary(&app.exec) {
                by_bin
                    .entry(bin.to_ascii_lowercase())
                    .or_insert(entry.clone());
                let stem = bin
                    .trim_end_matches("-stable")
                    .trim_end_matches("-beta")
                    .to_ascii_lowercase();
                by_bin.entry(stem).or_insert(entry.clone());
            }
            by_name
                .entry(app.name.to_ascii_lowercase())
                .or_insert(entry);
        }
        Self { by_bin, by_name }
    }

    /// Rozwiązuje nazwę wpisaną przez użytkownika (desktop id, nazwa, binarka).
    pub fn resolve(&self, query: &str) -> Option<(String, String)> {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return None;
        }
        self.by_bin
            .get(&q)
            .or_else(|| self.by_name.get(&q))
            .cloned()
            .or_else(|| {
                let base = Path::new(&q)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_ascii_lowercase())?;
                self.by_bin.get(&base).cloned()
            })
    }
}

fn exec_binary(exec: &str) -> Option<String> {
    let mut tokens = exec.split_whitespace();
    let mut first = tokens.next()?;
    while first.contains('=') || first == "env" {
        first = tokens.next()?;
    }
    let bin = Path::new(first).file_name()?.to_string_lossy().into_owned();
    (!bin.is_empty()).then_some(bin)
}

/// Skanuje `/proc` i PipeWire; zwraca aktywności per aplikacja.
pub fn scan_all() -> Vec<Observation> {
    let map = AppMap::load();
    let mut obs = scan_processes(&map);
    merge_pipewire(&mut obs, &pipewire_streams(), &map);
    obs
}

/// Skan z gotową mapą aplikacji i strumieniami PipeWire (daemon cache'uje jedno i drugie).
pub fn scan_with(map: &AppMap, streams: &[PipewireStream]) -> Vec<Observation> {
    let mut obs = scan_processes(map);
    merge_pipewire(&mut obs, streams, map);
    obs
}

/// Narzędzia systemowe/CLI, dla których sieć jest normalną częścią pracy —
/// nie pokazujemy dla nich modala (ich ruch nadal trafia do dziennika).
pub fn is_background_tool(app: &str) -> bool {
    matches!(
        app.to_ascii_lowercase().as_str(),
        "curl"
            | "wget"
            | "git"
            | "ssh"
            | "scp"
            | "sftp"
            | "rsync"
            | "pacman"
            | "apt"
            | "apt-get"
            | "dnf"
            | "flatpak"
            | "pag"
            | "emerge"
            | "cargo"
            | "rustc"
            | "node"
            | "npm"
            | "python"
            | "python3"
            | "pip"
            | "pip3"
    )
}

/// Skanuje procesy użytkownika (bez PipeWire).
pub fn scan_processes(map: &AppMap) -> Vec<Observation> {
    let remotes = public_remotes();
    let uid = unsafe { libc::getuid() };
    let mut by_app: BTreeMap<String, Observation> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_string_lossy().parse::<u32>().ok() else {
            continue;
        };
        let proc = PathBuf::from("/proc").join(pid.to_string());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if std::fs::metadata(&proc).map(|m| m.uid()).ok() != Some(uid) {
                continue;
            }
        }
        let exe = std::fs::read_link(proc.join("exe"))
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let comm = std::fs::read_to_string(proc.join("comm"))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        let cmd = std::fs::read(proc.join("cmdline"))
            .ok()
            .and_then(|b| {
                b.split(|c| *c == 0)
                    .find(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
            })
            .unwrap_or_default();
        if is_ignored(&comm, &exe) {
            continue;
        }
        let (app, display) = identify(map, &comm, &exe, &cmd);
        let mut mic = false;
        let mut camera = false;
        let mut remote = None;
        let Ok(fds) = std::fs::read_dir(proc.join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let target = target.to_string_lossy();
            if target.starts_with("/dev/snd/") && target.ends_with('c') {
                // Tylko urządzenia **capture** (`pcmC0D0c`); `pcmC0D0p` to odtwarzanie.
                mic = true;
            } else if target.starts_with("/dev/video") || target.starts_with("/dev/media") {
                camera = true;
            } else if let Some(inode) = socket_inode(&target) {
                if let Some(found) = remotes.get(&inode) {
                    remote.get_or_insert_with(|| found.clone());
                }
            }
        }
        let entry = by_app.entry(app.clone()).or_insert_with(|| Observation {
            app,
            name: display,
            ..Default::default()
        });
        entry.pids.insert(pid);
        if mic {
            entry.caps.insert(Capability::Microphone);
        }
        if camera {
            entry.caps.insert(Capability::Camera);
        }
        if let Some(found) = remote {
            entry.caps.insert(Capability::Network);
            entry.remote.get_or_insert(found);
        }
    }
    by_app.into_values().collect()
}

fn merge_pipewire(obs: &mut Vec<Observation>, streams: &[PipewireStream], map: &AppMap) {
    for stream in streams {
        let app = stream
            .pid
            .and_then(|pid| {
                let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                    .map(|s| s.trim().to_owned())
                    .unwrap_or_default();
                let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (!comm.is_empty() || !exe.is_empty()).then(|| identify(map, &comm, &exe, ""))
            })
            .or_else(|| {
                stream
                    .name
                    .as_deref()
                    .and_then(|name| map.resolve(name))
                    .map(|(app, display)| (app, display))
            })
            .or_else(|| {
                stream
                    .name
                    .as_deref()
                    .map(|n| (n.to_ascii_lowercase(), n.to_owned()))
            });
        let Some((app, display)) = app else {
            continue;
        };
        if is_trusted_app(&app) {
            continue;
        }
        if let Some(entry) = obs.iter_mut().find(|o| o.app == app) {
            entry.caps.insert(stream.capability);
        } else {
            let mut entry = Observation {
                app,
                name: display,
                ..Default::default()
            };
            entry.caps.insert(stream.capability);
            obs.push(entry);
        }
    }
}

fn socket_inode(target: &str) -> Option<u64> {
    target
        .strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

fn is_ignored(comm: &str, exe: &str) -> bool {
    const IGNORED: &[&str] = &[
        "luna-security",
        "luna-shell",
        "luna-session",
        "luna-keyring",
        "luna-polkit",
        "luna-eq",
        "luna-lock",
        "luna-accounts",
        "selene",
        "systemd",
        "dbus-daemon",
        "dbus-broker",
        "Xwayland",
        "pipewire",
        "wireplumber",
        "pulseaudio",
        "xdg-desktop-portal",
        "xdg-dbus-proxy",
        "gvfsd",
        "gnome-keyring",
        "xdg-permission-store",
    ];
    IGNORED.iter().any(|n| comm.eq_ignore_ascii_case(n))
        || exe.starts_with("/usr/local/bin/luna-")
        || exe.starts_with("/usr/bin/luna-")
        || exe.contains("/libexec/luna-")
}

fn identify(map: &AppMap, comm: &str, exe: &str, cmd: &str) -> (String, String) {
    for candidate in [exe, cmd, comm] {
        if candidate.is_empty() {
            continue;
        }
        if let Some(found) = map.resolve(candidate) {
            return found;
        }
        if let Some(base) = Path::new(candidate).file_name() {
            if let Some(found) = map.resolve(&base.to_string_lossy()) {
                return found;
            }
        }
    }
    let lower = format!("{comm} {exe} {cmd}").to_ascii_lowercase();
    for (needle, key, name) in [
        ("chrome", "google-chrome", "Google Chrome"),
        ("ecosia", "ecosia-browser", "Ecosia Browser"),
        ("chromium", "chromium", "Chromium"),
        ("microsoft-edge", "microsoft-edge", "Microsoft Edge"),
        ("brave", "brave-browser", "Brave"),
        ("vivaldi", "vivaldi", "Vivaldi"),
        ("firefox", "firefox", "Firefox"),
        ("librewolf", "librewolf", "LibreWolf"),
    ] {
        if lower.contains(needle) {
            return (key.to_owned(), name.to_owned());
        }
    }
    let key = if comm.is_empty() {
        "unknown".to_owned()
    } else {
        comm.to_ascii_lowercase()
    };
    let name = if comm.is_empty() {
        key.clone()
    } else {
        comm.to_owned()
    };
    (key, name)
}

/// Katalog logów (`~/.local/state/luna/security.log`) lub `None`.
pub fn log_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("luna/security.log"))
}

/// Dopisuje wpis do dziennika (bez wymuszania `fsync` — to tylko audyt).
pub fn log_line(line: &str) {
    let Some(path) = log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// Wczytuje ostatnie `n` linii dziennika (CLI `luna-security log`).
pub fn tail_log(n: usize) -> Vec<String> {
    let Some(path) = log_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines: Vec<&str> = text.lines().collect();
    if lines.len() > n {
        lines.drain(..lines.len() - n);
    }
    lines.into_iter().map(str::to_owned).collect()
}

/// Ustawia politykę dla `app` (używane przez CLI i daemon).
pub fn apply_decision(policy: &mut Policy, app: &str, cap: Capability, allow: bool) {
    if allow {
        policy.set_allow(app, cap);
        log_line(&format!(
            "{} decision allow app={app} cap={}",
            now(),
            cap.key()
        ));
    } else {
        policy.set_deny(app, cap);
        log_line(&format!(
            "{} decision deny app={app} cap={}",
            now(),
            cap.key()
        ));
    }
}

/// Czy aplikacja ma już decyzję (skrót dla testów/CLI).
pub fn has_decision(policy: &Policy, app: &str, cap: Capability) -> Option<bool> {
    if policy.allows(app, cap) {
        Some(true)
    } else if policy.denies(app, cap) {
        Some(false)
    } else {
        None
    }
}

/// Zbiór ignorowanych nazw (publiczne dla testów).
pub fn ignored_process(name: &str) -> bool {
    is_ignored(name, "")
        || is_trusted_app(name)
        || matches!(
            name.to_ascii_lowercase().as_str(),
            "systemd" | "dbus-daemon" | "selene" | "luna-shell"
        )
}

/// Czy `app`/`cap` powinno dostać modal (brak decyzji i nie jest zaufane).
pub fn needs_consent(policy: &Policy, app: &str, cap: Capability) -> bool {
    !is_trusted_app(app)
        && !is_background_tool(app)
        && !policy.allows(app, cap)
        && !policy.denies(app, cap)
}

/// Nazwa aplikacji do wyświetlenia, gdy mamy tylko klucz.
pub fn display_name(app: &str) -> String {
    AppMap::load()
        .resolve(app)
        .map(|(_, name)| name)
        .unwrap_or_else(|| app.to_owned())
}

/// Czy `pid` należy do bieżącego użytkownika (używane w diagnostyce).
pub fn pid_owned_by_us(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = unsafe { libc::getuid() };
        std::fs::metadata(format!("/proc/{pid}"))
            .map(|m| m.uid() == uid)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_roundtrip() {
        for cap in Capability::ALL {
            assert_eq!(Capability::from_key(cap.key()), Some(cap));
        }
        assert_eq!(
            Capability::from_key("mikrofon"),
            Some(Capability::Microphone)
        );
        assert_eq!(Capability::from_key("nonsense"), None);
    }

    #[test]
    fn parses_hex_endpoints() {
        let (ip, port) = parse_hex_endpoint("0100007F:1F90").unwrap();
        assert_eq!(ip.to_string(), "127.0.0.1");
        assert_eq!(port, 8080);
        // 142.250.75.238:443 w zapisie little-endian.
        let (ip, port) = parse_hex_endpoint("EE4BFA8E:01BB").unwrap();
        assert_eq!(ip.to_string(), "142.250.75.238");
        assert_eq!(port, 443);
    }

    #[test]
    fn private_addresses_do_not_trigger_consent() {
        for (ip, port) in [
            ("127.0.0.1", 443),
            ("10.1.2.3", 443),
            ("172.16.0.1", 443),
            ("192.168.0.1", 443),
            ("169.254.1.1", 443),
            ("::1", 443),
            ("fe80::1", 443),
            ("fd00::1", 443),
        ] {
            assert!(!is_public_remote(ip.parse().unwrap(), port), "{ip}");
        }
        assert!(is_public_remote("1.1.1.1".parse().unwrap(), 443));
        assert!(is_public_remote("2606:4700::1111".parse().unwrap(), 443));
    }

    #[test]
    fn policy_roundtrip_and_precedence() {
        let mut p = Policy::default();
        p.set_allow("firefox", Capability::Network);
        p.set_allow("firefox", Capability::Microphone);
        assert!(p.allows("firefox", Capability::Network));
        assert!(!p.denies("firefox", Capability::Network));
        p.set_deny("firefox", Capability::Network);
        assert!(!p.allows("firefox", Capability::Network));
        assert!(p.denies("firefox", Capability::Network));
        let parsed = Policy::parse(&p.to_text());
        assert_eq!(parsed, p);
    }

    #[test]
    fn policy_rejects_malformed_lines() {
        let p = Policy::parse("allow=firefox:network\nbroken\ndeny=:mic\nallow=x:\n");
        assert!(p.allows("firefox", Capability::Network));
        assert!(p.entries().len() == 1);
        assert!(!p.denies("x", Capability::Microphone));
    }

    #[test]
    fn snapshot_roundtrip() {
        let snap = Snapshot {
            activities: vec![Activity {
                app: "firefox".into(),
                name: "Firefox".into(),
                caps: [Capability::Network, Capability::Microphone]
                    .into_iter()
                    .collect(),
                pids: [12, 34].into_iter().collect(),
                remote: Some("1.1.1.1:443".into()),
                status: Status::Ask,
                since: 42,
            }],
            prompts: vec![Prompt {
                id: "firefox:network".into(),
                app: "firefox".into(),
                name: "Firefox".into(),
                capability: Capability::Network,
                remote: Some("1.1.1.1:443".into()),
                created: 42,
            }],
        };
        assert_eq!(Snapshot::parse(&snap.to_text()), snap);
    }

    #[test]
    fn snapshot_ignores_garbage() {
        assert_eq!(
            Snapshot::parse("activity\ttoo\tfew\nprompt\tx\n# komentarz"),
            Snapshot::default()
        );
    }

    #[test]
    fn consent_skips_trusted_components_and_existing_decisions() {
        let mut p = Policy::default();
        assert!(!needs_consent(&p, "luna-shell", Capability::Network));
        assert!(needs_consent(&p, "firefox", Capability::Network));
        p.set_allow("firefox", Capability::Network);
        assert!(!needs_consent(&p, "firefox", Capability::Network));
    }

    #[test]
    fn browser_aliases_are_stable() {
        let map = AppMap {
            by_bin: HashMap::new(),
            by_name: HashMap::new(),
        };
        let (key, name) = identify(&map, "chrome", "/opt/google/chrome/chrome", "");
        assert_eq!(key, "google-chrome");
        assert_eq!(name, "Google Chrome");
        let (key, _) = identify(&map, "ecosia-browser", "/opt/ecosia/ecosia", "");
        assert_eq!(key, "ecosia-browser");
    }
}
