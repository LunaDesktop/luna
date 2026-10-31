//! `luna-eq` — sterowanie korektorem Luny z linii poleceń.
//!
//! Użycie:
//! ```text
//! luna-eq start          # włącz, jeśli w konfiguracji jest enabled = true (sesja)
//! luna-eq apply          # zastosuj zapisane ustawienia (restart łańcucha)
//! luna-eq stop           # wyłącz i przywróć poprzednie wyjście
//! luna-eq status         # co jest włączone i na jakim wyjściu
//! luna-eq preset bass    # zmień krzywą i zastosuj
//! luna-eq band 5 6       # pasmo (0–9) w dB i zastosuj
//! ```
//!
//! Sesja (`luna-session`) woła `luna-eq start`, więc po wylogowaniu korektor
//! wraca automatycznie, bez otwierania ustawień.

use std::process::ExitCode;

use luna::equalizer::{self, Equalizer, Preset, BAND_LABELS};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("status");

    match command {
        "start" => {
            let eq = Equalizer::load();
            if !eq.enabled {
                return ExitCode::SUCCESS;
            }
            report(equalizer::apply(&eq));
        }
        "apply" => {
            let eq = Equalizer::load();
            report(equalizer::apply(&eq));
        }
        "stop" | "disable" => {
            // Wyłączenie z CLI ma też wyłączyć przełącznik w ustawieniach.
            let mut eq = Equalizer::load();
            eq.enabled = false;
            if let Err(err) = eq.save() {
                eprintln!("luna-eq: nie można zapisać ustawień: {err}");
            }
            report(equalizer::disable());
        }
        "preset" => {
            let Some(name) = args.get(1) else {
                eprintln!("luna-eq: podaj nazwę krzywej ({})", preset_names());
                return ExitCode::FAILURE;
            };
            let Some(preset) = Preset::from_key(name) else {
                eprintln!("luna-eq: nieznana krzywa „{name}” ({})", preset_names());
                return ExitCode::FAILURE;
            };
            let mut eq = Equalizer::load();
            eq.enabled = true;
            eq.set_preset(preset);
            report(equalizer::save_and_apply(&eq));
        }
        "band" => {
            let (Some(index), Some(gain)) = (args.get(1), args.get(2)) else {
                eprintln!("luna-eq: użycie: luna-eq band <0-9> <dB>");
                return ExitCode::FAILURE;
            };
            let (Ok(index), Ok(gain)) = (index.parse::<usize>(), gain.parse::<f32>()) else {
                eprintln!("luna-eq: numer pasma i wzmocnienie muszą być liczbami");
                return ExitCode::FAILURE;
            };
            if index >= BAND_LABELS.len() {
                eprintln!("luna-eq: pasmo {index} poza zakresem 0–9");
                return ExitCode::FAILURE;
            }
            let mut eq = Equalizer::load();
            eq.enabled = true;
            eq.set_band(index, gain);
            report(equalizer::save_and_apply(&eq));
        }
        "status" => print_status(),
        "-h" | "--help" | "help" => print_help(),
        other => {
            eprintln!("luna-eq: nieznane polecenie „{other}”");
            print_help();
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

fn report(result: Result<(), String>) {
    if let Err(err) = result {
        eprintln!("luna-eq: {err}");
        std::process::exit(1);
    }
}

fn preset_names() -> String {
    Preset::ALL
        .iter()
        .map(|p| p.key())
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_status() {
    let eq = Equalizer::load();
    let status = equalizer::status();
    println!(
        "wtyczka: {}",
        equalizer::plugin_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "brak".to_owned())
    );
    println!("włączony w ustawieniach: {}", eq.enabled);
    println!(
        "łańcuch działa: {}",
        if status.eq_id.is_some() { "tak" } else { "nie" }
    );
    println!(
        "wyjście sprzętowe: {}",
        equalizer::active_output().unwrap_or_else(|| "—".to_owned())
    );
    println!("krzywa: {}", eq.detected_preset().key());
    println!("przedwzmacniacz: {:+.1} dB", eq.preamp);
    for (label, gain) in BAND_LABELS.iter().zip(eq.bands.iter()) {
        println!("  {label:>7} {:+.1} dB", gain);
    }
    if status.sinks.is_empty() {
        println!("uwaga: PipeWire nie zgłasza żadnego wyjścia");
    }
}

fn print_help() {
    println!(
        "\
luna-eq — korektor Luny (PipeWire + LADSPA)

  luna-eq start          włącz, jeśli zapisano enabled = true (używa sesja)
  luna-eq apply          zastosuj zapisane ustawienia
  luna-eq stop           wyłącz i przywróć poprzednie wyjście
  luna-eq status         pokaż stan korektora
  luna-eq preset <nazwa> wybierz krzywą: {}
  luna-eq band <0-9> <dB> ustaw pasmo",
        preset_names()
    );
}
