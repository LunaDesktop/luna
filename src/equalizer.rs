//! Korektor graficzny (PipeWire + wtyczka LADSPA `luna-eq`).
//!
//! Ustawienia są źródłem prawdy dla panelu i centrum ustawień: panel pokazuje
//! głośność **domyślnego wyjścia** (a po włączeniu korektora jest nim wirtualne
//! wyjście `luna_eq_sink`), więc oba interfejsy zawsze mówią o tym samym.
//!
//! ## Jak to działa
//! 1. [`render_chain_config`] generuje prywatną konfigurację `~/.config/pipewire/luna-equalizer.conf`
//!    (własny plik, nie wspólny `filter-chain.conf.d`) z łańcuchem filtrów LADSPA.
//! 2. [`apply`] uruchamia `pipewire -c luna-equalizer.conf` w tle (nowa sesja przez `setsid`),
//!    czeka na węzeł [`NODE_NAME`] i ustawia go jako domyślne wyjście.
//! 3. Wtyczka LADSPA (`libluna-eq.so`) omija zepsuty w części dystrybucji plugin
//!    `builtin` (libsndfile/LAME), bo `filter-chain` ładuje wtedy
//!    `libspa-filter-graph-plugin-ladspa.so`, który nie ma tej zależności.
//!
//! Wyjście łańcucha (`playback.props`) ma ustawiony `node.target` na sprzętowe
//! wyjście, więc WirePlumber sam podłącza je do karty. Zmiana pasm wymaga
//! restartu łańcucha (kontrolki LADSPA nie są wystawione jako parametry węzła);
//! dlatego UI wysyła zmiany dopiero po ustabilizowaniu suwaka.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

pub use libluna_core::equalizer::{Equalizer, Preset, BANDS, BAND_LABELS, MAX_GAIN_DB};

pub use libluna_core::equalizer::CHAIN_FILE;
/// Nazwa węzła wirtualnego wyjścia korektora.
pub const NODE_NAME: &str = "luna_eq_sink";

/// Znacznik procesu łańcucha (`pinewire -c <plik>`); unikalny, żeby nie zabić
/// innych filtrów użytkownika.
pub const CHAIN_ARGS: [&str; 2] = ["-c", CHAIN_FILE];

/// Wykryte urządzenie audio (wyjście albo wejście).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: u32,
    pub description: String,
    pub is_default: bool,
}

/// Migawka `wpctl status` (sekcja Audio).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioStatus {
    pub sinks: Vec<Device>,
    pub sources: Vec<Device>,
    /// Identyfikator węzła korektora, gdy łańcuch działa.
    pub eq_id: Option<u32>,
}

impl AudioStatus {
    /// Sprzętowe wyjścia (bez wirtualnego wyjścia korektora).
    pub fn hardware_sinks(&self) -> impl Iterator<Item = &Device> {
        self.sinks
            .iter()
            .filter(|d| d.description != NODE_NAME && !d.description.starts_with("Luna Equalizer"))
    }
}

/// Czy wtyczka korektora jest zainstalowana.
pub fn available() -> bool {
    plugin_path().is_some()
}

/// Ścieżka wtyczki LADSPA (zainstalowanej w `$prefix/lib/luna` albo w budowanym repo).
pub fn plugin_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LUNA_EQ_PLUGIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            if let Some(prefix) = bin_dir.parent() {
                candidates.push(prefix.join("lib/luna/libluna-eq.so"));
                candidates.push(prefix.join("lib64/luna/libluna-eq.so"));
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(&home).join(".local/lib/luna/libluna-eq.so"));
    }
    candidates.extend([
        PathBuf::from("/usr/local/lib/luna/libluna-eq.so"),
        PathBuf::from("/usr/lib/luna/libluna-eq.so"),
        PathBuf::from("/usr/lib64/luna/libluna-eq.so"),
    ]);
    candidates.into_iter().find(|p| p.is_file())
}

/// Katalog konfiguracji PipeWire użytkownika.
fn pipewire_config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("pipewire"))
}

/// Plik konfiguracji generowany przez Lunę (własny, nie wspólny `conf.d`).
pub fn chain_config_file() -> Option<PathBuf> {
    Some(pipewire_config_dir()?.join(CHAIN_FILE))
}

/// Plik z ostatnim sprzętowym wyjściem (do przywrócenia po wyłączeniu korektora).
fn output_file() -> Option<PathBuf> {
    libluna_core::config::config_file("equalizer-output.conf")
}

/// Uruchamia `wpctl status` i parsuje wynik (puste przy braku PipeWire).
pub fn status() -> AudioStatus {
    match run("wpctl", &["status"]) {
        Some(out) => parse_status(&out),
        None => AudioStatus::default(),
    }
}

/// Czy łańcuch korektora działa.
pub fn running() -> bool {
    status().eq_id.is_some()
}

/// Ustawia domyślne wyjście (albo wejście) po identyfikatorze z `wpctl`.
pub fn set_default(id: u32) -> bool {
    Command::new("wpctl")
        .args(["set-default", &id.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Nazwa węzła danego id (`node.name` z `wpctl inspect`).
pub fn node_name(id: u32) -> Option<String> {
    let out = run("wpctl", &["inspect", &id.to_string()])?;
    parse_inspect_name(&out)
}

/// Identyfikator węzła o zadanej `node.name` (`pw-cli ls Node`).
pub fn node_id_by_name(name: &str) -> Option<u32> {
    let out = run("pw-cli", &["ls", "Node"])?;
    parse_node_ids(&out, name)
}

/// Sprzętowe wyjście zapamiętane przy włączaniu korektora.
pub fn remembered_output() -> Option<String> {
    let path = output_file()?;
    let text = std::fs::read_to_string(path).ok()?;
    libluna_core::config::parse_pairs(&text)
        .into_iter()
        .find(|(k, _)| k == "output")
        .and_then(|(_, v)| libluna_core::config::clean_value(&v))
}

fn remember_output(name: &str) {
    let Some(path) = output_file() else { return };
    let text = format!("# Wyjście przywracane po wyłączeniu korektora Luny.\noutput = {name}\n");
    let _ = libluna_core::config::save_atomic(&path, &text);
}

/// Aktualnie wybrane sprzętowe wyjście (nawet gdy gra przez korektor).
pub fn active_output() -> Option<String> {
    let current = node_name_of_default()?;
    if current == NODE_NAME {
        remembered_output()
    } else {
        Some(current)
    }
}

fn node_name_of_default() -> Option<String> {
    let out = run("wpctl", &["inspect", "@DEFAULT_AUDIO_SINK@"])?;
    parse_inspect_name(&out)
}

/// Włącza/wyłącza korektor zgodnie z ustawieniami i odświeża łańcuch.
///
/// Zwraca komunikat dla UI: sukces albo powód, dlaczego się nie udało.
pub fn apply(eq: &Equalizer) -> Result<(), String> {
    if !eq.enabled {
        return disable();
    }
    let Some(plugin) = plugin_path() else {
        return Err(
            "Brak wtyczki luna-eq (zainstaluj pakiet luna lub uruchom scripts/install.sh)"
                .to_owned(),
        );
    };
    let Some(config) = chain_config_file() else {
        return Err("Brak katalogu konfiguracji użytkownika".to_owned());
    };

    // Sprzętowe wyjście: zapamiętane (gdy już graliśmy przez korektor) albo bieżące.
    // Po zalogowaniu PipeWire odpowiada chwilę po sesji — czekamy, zamiast od razu
    // zgłaszać „brak wyjścia”.
    let mut current = node_name_of_default();
    let deadline = Instant::now() + Duration::from_secs(3);
    while current.is_none() && Instant::now() < deadline {
        sleep(Duration::from_millis(200));
        current = node_name_of_default();
    }
    let target = match current.as_deref() {
        Some(NODE_NAME) => remembered_output().or_else(|| {
            status()
                .hardware_sinks()
                .next()
                .and_then(|d| node_name(d.id))
        }),
        Some(other) => Some(other.to_owned()),
        None => None,
    };
    let Some(target) = target else {
        return Err("Nie wykryto żadnego wyjścia audio".to_owned());
    };
    if target == NODE_NAME {
        return Err("Nie wykryto sprzętowego wyjścia audio".to_owned());
    }
    remember_output(&target);

    let text = render_chain_config(eq, &plugin, &target);
    libluna_core::config::save_atomic(&config, &text)
        .map_err(|err| format!("Nie można zapisać konfiguracji: {err}"))?;

    // Głośność obowiązuje dla domyślnego wyjścia — po przełączeniu na węzeł korektora
    // trzeba ją na nim odtworzyć, inaczej dźwięk „wskakuje” na 100%.
    let saved_volume = default_volume();

    stop_chain();
    spawn_chain();
    match wait_for_node() {
        Some(id) => {
            if let Some((volume, muted)) = saved_volume {
                set_node_volume(id, volume, muted);
            }
            Ok(())
        }
        None => Err("Korektor nie wystartował (sprawdź ~/.cache/luna/equalizer.log)".to_owned()),
    }
}

/// Wyłącza łańcuch, usuwa konfigurację i przywraca poprzednie wyjście.
pub fn disable() -> Result<(), String> {
    stop_chain();
    if let Some(config) = chain_config_file() {
        let _ = std::fs::remove_file(config);
    }
    if let Some(name) = remembered_output() {
        if let Some(id) = node_id_by_name(&name) {
            set_default(id);
        }
    }
    Ok(())
}

/// Zapisuje ustawienia i (jeśli trzeba) przełącza łańcuch.
pub fn save_and_apply(eq: &Equalizer) -> Result<(), String> {
    eq.save()
        .map_err(|err| format!("Nie można zapisać ustawień: {err}"))?;
    apply(eq)
}

fn spawn_chain() {
    let log_path = log_file();
    let stderr = log_path
        .as_ref()
        .and_then(|p| {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
        })
        .map(Stdio::from)
        .unwrap_or_else(Stdio::null);
    // `setsid -f` odcina proces od sesji terminala: łańcuch żyje dalej po zamknięciu
    // ustawień i po wylogowaniu ginie wraz z pipewire, do którego był podłączony.
    let _ = Command::new("setsid")
        .arg("-f")
        .arg("pipewire")
        .arg(CHAIN_ARGS[0])
        .arg(CHAIN_ARGS[1])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn();
}

fn stop_chain() {
    let _ = Command::new("pkill")
        .args(["-f", "pipewire -c luna-equalizer.conf"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    // Daj PipeWire chwilę na zwolnienie nazwy węzła przed ponownym startem.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && status().eq_id.is_some() {
        sleep(Duration::from_millis(100));
    }
}

fn wait_for_node() -> Option<u32> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(id) = status().eq_id {
            // WirePlumber widzi nowy węzeł od razu, ale domyślne wyjście ustawiamy
            // dopiero, gdy węzeł istnieje — inaczej `set-default` cicho nie działa.
            if set_default(id) {
                return Some(id);
            }
        }
        sleep(Duration::from_millis(150));
    }
    None
}

/// Głośność domyślnego wyjścia `(0.0–1.0, wyciszone)`.
fn default_volume() -> Option<(f32, bool)> {
    let out = run("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])?;
    parse_volume(&out)
}

fn parse_volume(text: &str) -> Option<(f32, bool)> {
    let muted = text.contains("MUTED");
    let value = text
        .split_whitespace()
        .find_map(|word| word.parse::<f32>().ok())?;
    Some((value.clamp(0.0, 1.5), muted))
}

fn set_node_volume(id: u32, volume: f32, muted: bool) {
    let _ = Command::new("wpctl")
        .args(["set-volume", &id.to_string(), &format!("{volume:.2}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = Command::new("wpctl")
        .args(["set-mute", &id.to_string(), if muted { "1" } else { "0" }])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn log_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("luna/equalizer.log"))
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Generuje konfigurację PipeWire z łańcuchem LADSPA.
///
/// `target` to `node.name` sprzętowego wyjścia — to na nie trafia dźwięk po
/// korekcji. Wartości sterujące są wstawiane jako liczby (bez locale).
pub fn render_chain_config(eq: &Equalizer, plugin: &Path, target: &str) -> String {
    let plugin = plugin.display();
    let target = sanitize_name(target);
    let mut controls = format!("{{ \"Preamp\" = {:.2}", eq.preamp);
    for (label, gain) in BAND_LABELS.iter().zip(eq.bands.iter()) {
        controls.push_str(&format!(" \"{label}\" = {gain:.2}"));
    }
    controls.push_str(" }");
    format!(
        "\
# Wygenerowane przez Lunę — nie edytuj ręcznie (zmiany nadpisze centrum ustawień).
# Korektor: 10 pasm + przedwzmacniacz, wtyczka LADSPA (bez pluginu `builtin`).
context.properties = {{
    log.level = 0
}}

context.spa-libs = {{
    audio.convert.* = audioconvert/libspa-audioconvert
    support.*       = support/libspa-support
}}

context.modules = [
    {{ name = libpipewire-module-rt flags = [ ifexists nofail ] }}
    {{ name = libpipewire-module-protocol-native }}
    {{ name = libpipewire-module-client-node }}
    {{ name = libpipewire-module-adapter }}
    {{ name = libpipewire-module-filter-chain
        args = {{
            node.description = \"Luna Equalizer\"
            media.name       = \"Luna Equalizer\"
            audio.channels   = 2
            audio.position   = [ FL FR ]
            filter.graph = {{
                nodes = [
                    {{ type = ladspa
                       plugin = \"{plugin}\"
                       label = luna_eq
                       control = {controls}
                    }}
                ]
            }}
            capture.props = {{
                node.name        = \"{NODE_NAME}\"
                node.description = \"Luna Equalizer\"
                media.class      = Audio/Sink
            }}
            playback.props = {{
                node.name   = \"luna_eq_output\"
                node.target = \"{target}\"
            }}
        }}
    }}
]
"
    )
}

/// `node.name` trafia do cytowanego stringu w konfiguracji — dopuszczamy tylko
/// bezpieczne znaki (PipeWire i tak używa `[A-Za-z0-9._-]`).
fn sanitize_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(128)
        .collect()
}

/// Parsuje sekcję Audio z `wpctl status`.
pub fn parse_status(text: &str) -> AudioStatus {
    let mut status = AudioStatus::default();
    let mut section = Section::Other;
    let mut in_audio = false;
    for raw in text.lines() {
        let line = raw.trim();
        if !line.is_empty() && !line.starts_with(['│', '├', '└', '─']) && !raw.starts_with(' ')
        {
            // Nagłówek najwyższego poziomu: „Audio”, „Video”, „Bandwidth”…
            in_audio = line == "Audio";
            section = Section::Other;
            continue;
        }
        if !in_audio {
            continue;
        }
        let body = line.trim_start_matches(['│', '├', '└', '─', ' ']);
        if body.starts_with("Sinks:") {
            section = Section::Sinks;
            continue;
        }
        if body.starts_with("Sources:") {
            section = Section::Sources;
            continue;
        }
        if body.starts_with("Filters:") {
            section = Section::Filters;
            continue;
        }
        if body.is_empty() || body.starts_with('-') {
            continue;
        }
        let Some((id, description, is_default)) = parse_device_line(body) else {
            continue;
        };
        match section {
            Section::Sinks => status.sinks.push(Device {
                id,
                description,
                is_default,
            }),
            Section::Sources => status.sources.push(Device {
                id,
                description,
                is_default,
            }),
            Section::Filters => {
                if description == NODE_NAME || description == "Luna Equalizer" {
                    status.eq_id = Some(id);
                }
            }
            Section::Other => {}
        }
    }
    status
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Other,
    Sinks,
    Sources,
    Filters,
}

/// `*   46. Opis [vol: 0.40]` → `(46, "Opis", true)`.
fn parse_device_line(line: &str) -> Option<(u32, String, bool)> {
    let mut rest = line.trim();
    let is_default = rest.starts_with('*');
    if is_default {
        rest = rest[1..].trim_start();
    }
    let (number, description) = rest.split_once('.')?;
    let id: u32 = number.trim().parse().ok()?;
    let mut description = description.trim();
    if let Some(pos) = description.find(" [") {
        description = &description[..pos];
    }
    Some((id, description.trim().to_owned(), is_default))
}

fn parse_inspect_name(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim_start_matches(['*', ' ', '\t']);
        if let Some(value) = line.strip_prefix("node.name") {
            let value = value.trim_start_matches([' ', '=']);
            let value = value.trim().trim_matches('"');
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

/// Wyciąga id węzła o podanej nazwie z `pw-cli ls Node`.
fn parse_node_ids(text: &str, name: &str) -> Option<u32> {
    let mut current: Option<u32> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("id ") {
            current = rest
                .split(',')
                .next()
                .and_then(|n| n.trim().parse::<u32>().ok());
            continue;
        }
        if let Some(rest) = line.strip_prefix("node.name") {
            let value = rest.trim_start_matches([' ', '=']).trim().trim_matches('"');
            if value == name {
                return current;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
PipeWire 'pipewire-0' [1.6.8, patryks@host, cookie:4027179111]
 └─ Clients:
        33. WirePlumber

Audio
 ├─ Devices:
 │      48. TU116 High Definition Audio Controller [alsa]
 ├─ Sinks:
 │  *   46. Starship/Matisse HD Audio Controller Analogowe stereo [vol: 0.40]
 │      57. TU116 High Definition Audio Controller Digital Stereo (HDMI 2) [vol: 0.40]
 ├─ Sources:
 │      35. SM900T Microphone Mono              [vol: 1.00]
 ├─ Filters:
 │    - filter-chain-747749-8
 │      73. luna_eq_sink                                                 [Audio/Sink]
 └─ Streams:

Video
 ├─ Sinks:
";

    #[test]
    fn parses_sinks_sources_and_eq_node() {
        let status = parse_status(SAMPLE);
        assert_eq!(status.sinks.len(), 2);
        assert_eq!(status.sinks[0].id, 46);
        assert!(status.sinks[0].is_default);
        assert_eq!(
            status.sinks[0].description,
            "Starship/Matisse HD Audio Controller Analogowe stereo"
        );
        assert_eq!(status.sources.len(), 1);
        assert_eq!(status.sources[0].id, 35);
        assert_eq!(status.eq_id, Some(73));
    }

    #[test]
    fn hardware_sinks_skip_equalizer() {
        let mut status = parse_status(SAMPLE);
        status.sinks.push(Device {
            id: 90,
            description: "Luna Equalizer".to_owned(),
            is_default: true,
        });
        let ids: Vec<u32> = status.hardware_sinks().map(|d| d.id).collect();
        assert_eq!(ids, vec![46, 57]);
    }

    #[test]
    fn parses_inspect_name() {
        let out = "id 46, type PipeWire:Interface:Node\n  * media.class = \"Audio/Sink\"\n  * node.name = \"alsa_output.pci-0000_0c_00.4.analog-stereo\"\n";
        assert_eq!(
            parse_inspect_name(out).as_deref(),
            Some("alsa_output.pci-0000_0c_00.4.analog-stereo")
        );
    }

    #[test]
    fn parses_node_id_by_name() {
        let out = "\tid 46, type PipeWire:Interface:Node/3\n \t\tnode.name = \"alsa_output.pci\"\n\tid 75, type PipeWire:Interface:Node/3\n \t\tnode.name = \"luna_eq_sink\"\n";
        assert_eq!(parse_node_ids(out, "luna_eq_sink"), Some(75));
        assert_eq!(parse_node_ids(out, "brak"), None);
    }

    #[test]
    fn renders_valid_chain_config() {
        let mut eq = Equalizer::default();
        eq.enabled = true;
        eq.preamp = -1.5;
        eq.bands = [1.0, -2.0, 0.0, 0.0, 0.5, 3.0, 0.0, 0.0, -4.0, 0.0];
        let text = render_chain_config(
            &eq,
            Path::new("/usr/lib/luna/libluna-eq.so"),
            "alsa_output.x",
        );
        assert!(text.contains("plugin = \"/usr/lib/luna/libluna-eq.so\""));
        assert!(text.contains("label = luna_eq"));
        assert!(text.contains("\"Preamp\" = -1.50"));
        assert!(text.contains("\"31 Hz\" = 1.00"));
        assert!(text.contains("\"8 kHz\" = -4.00"));
        assert!(text.contains("node.name        = \"luna_eq_sink\""));
        assert!(text.contains("node.target = \"alsa_output.x\""));
        assert!(text.contains("audio.channels   = 2"));
        // Konfiguracja nie może zawierać śmieci z nazwy węzła.
        let weird = render_chain_config(&eq, Path::new("/p.so"), "a\"; rm -rf /");
        assert!(weird.contains("node.target = \"arm-rf\""));
    }

    #[test]
    fn parses_wpctl_volume() {
        assert_eq!(parse_volume("Volume: 0.40\n"), Some((0.40, false)));
        assert_eq!(parse_volume("Volume: 0.40 [MUTED]\n"), Some((0.40, true)));
        assert_eq!(parse_volume("Volumen: 1.00\n"), Some((1.0, false)));
        assert_eq!(parse_volume("brak\n"), None);
    }

    #[test]
    fn preset_round_trip_matches_bands() {
        let mut eq = Equalizer::default();
        for preset in Preset::ALL {
            eq.set_preset(preset);
            assert_eq!(eq.detected_preset(), preset);
        }
    }
}
