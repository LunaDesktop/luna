/// Powłoka Luny na żywo — pasek, launcher, tray, OSD, powiadomienia i wysuwane panele.
///
/// Interakcja:
/// - Klik w **burger** otwiera płynnie launcher (wysuwany z góry z przyciemnieniem tła).
///   **PPM na aplikacji** otwiera menu kontekstowe (ulubione, administrator, kopiowanie
///   polecenia, odinstalowanie przez Sklep); nowe pakiety pojawiają się bez restartu.
/// - Klik w **zegar** na środku paska otwiera wysuwany kalendarz oraz **centrum
///   powiadomień** (historia, „Wyczyść”, tryb „nie przeszkadzać”).
/// - Ikony **tray’a** (StatusNotifierItem) obsługują lewy/środkowy/prawy klik;
///   nadmiar ikon trafia do menu overflow `»`.
/// - Klik w **suwaki**, głośność lub baterię otwiera wspólny **Quick Panel**
///   (przełączniki, dźwięk, jasność, media, akcje sesji i Ustawienia).
/// - Klik w przełącznik **Wi-Fi** otwiera listę sieci (jedyne miejsce wyboru sieci).
/// - Kliknięcie **poza** aktywnym panelem/launcherem lub naciśnięcie **Esc** zamyka go płynnie.
use std::error::Error;
use std::process::Command;
use std::time::Instant;

use libluna_core::clock_state::{self, ClockActivity};
use libluna_core::{ButtonState, Event, IconSettings, PointerButton, Rect, ThemePreference};
use libluna_platform::{run, AppConfig, Frame, InputRegion, LayerConfig};
use libluna_widgets::{Hover, IconName, PaintCtx};
use luna::calendar;
use luna::clock_widget;
use luna::dbus_notifications::{Incoming, NotificationsBus};
use luna::notifications::{load_placement, Notification, Notifications};
use luna::osd::Osd;
use luna::panel::{
    self, ActivePopup, App, CalendarAction, Launcher, PanelState, Toggle, ToggleAction, WifiAction,
    LAUNCHER_H, LAUNCHER_W, THICKNESS,
};
use luna::theme_schedule::ThemeSchedule;

/// Wysokość powierzchni „chrome” (pasek + launcher + OSD/powiadomienia).
const CHROME_H: u32 = 680;

/// Jednostka przewijania w `wl_pointer.axis`: jeden „ząbek” kółka = tyle pikseli
/// powierzchni (patrz `selene::seat::WHEEL_STEP_PX`). Musi być z tym zgodna.
const SCROLL_UNIT: f64 = 20.0;
/// O ile pikseli przewijamy listę/siatkę launchera za jeden ząbek kółka.
/// ~64 px to około jeden wiersz listy — jedno kliknięcie kółka = jeden wiersz.
const LAUNCHER_SCROLL_PER_NOTCH: f64 = 64.0;

fn main() -> Result<(), Box<dyn Error>> {
    // Język z Ustawień Luny (`~/.config/luna/locale.conf`) ma pierwszeństwo
    // przed `LANG`; cała powłoka korzysta z jednego globalnego katalogu.
    libluna_i18n::set_locale(libluna_i18n::preferred_locale());
    libluna_i18n::load_catalogs_from_dirs();
    let system_dark = std::env::var("LUNA_SYSTEM_DARK")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false);
    let cfg = AppConfig::new("Luna Shell")
        .app_id("eu.paganlinux.Luna.Shell")
        .layer(LayerConfig::top_chrome(
            "luna-shell",
            THICKNESS as u32,
            CHROME_H,
        ))
        .transparent(true)
        .system_dark(system_dark)
        // Esc zamyka launcher/menu; nie może kończyć powłoki.
        .escape_to_exit(false);

    let mut state = PanelState::sample();
    let desktop = luna::apps::scan();
    let mut apps = if desktop.is_empty() {
        App::sample()
    } else {
        App::from_desktop(&desktop)
    };
    let mut launcher = Launcher::new();
    // Przypięte i ostatnio używane aplikacje przetrwają przelogowanie.
    luna::launcher_conf::load_into(&mut launcher);
    // Monitor prywatności: stan czytamy z pliku runtime (daemon `luna-security`).
    let mut security_snapshot = luna::security::load_snapshot();
    let mut security_stamp = luna::security::state_stamp();
    let mut security_check = Instant::now();
    let mut security_hover: Option<panel::SecurityChoice> = None;
    // Menu kontekstowe aplikacji w launcherze (PPM) — otwierane nad aktywnym launcherm.
    let mut app_menu: Option<AppMenuState> = None;
    let mut app_menu_hover: Option<usize> = None;
    // Znacznik katalogów `.desktop` — po instalacji pakietu lista aplikacji
    // odświeża się sama, bez restartu powłoki.
    let mut apps_stamp = luna::apps::stamp();
    let mut apps_checked = Instant::now();
    // Jedno miejsce śledzenia kursora dla paska, launchera i wszystkich popupów.
    let mut hover = Hover::new();

    // Język mógł zmienić się w Ustawieniach — odbuduj etykiety, które są cachowane.
    let mut animating_popup = ActivePopup::None;
    let mut anim_progress: f64 = 0.0;
    let mut target_progress: f64 = 0.0;
    // Menu kontekstowe przycisku taskbara (PPM): zadanie, etykiety i akcje.
    let mut task_menu: Option<TaskMenuState> = None;
    let mut task_menu_hover: Option<usize> = None;
    // Menu kontekstowe ikony tray’a (DBusMenu).
    let mut tray_context: Option<TrayContextState> = None;
    // Wiersz centrum powiadomień pod kursorem (podświetlenie).
    let mut notify_hover: Option<usize> = None;
    let mut last_menu_sig = Default::default();
    // Diagnostyka (`LUNA_STATS=1`): zmiany otwartego popupu (launcher, sieć, dźwięk…).
    let mut logged_popup = ActivePopup::None;

    let mut ui_language = libluna_i18n::locale().language().to_owned();

    // Data i kalendarz
    let mut cal_year = 2026i64;
    let mut cal_month = 10u32;
    let mut cal_selected: Option<u32> = None;
    // Wydarzenia z `luna-clock` (tylko odczyt; plik jest lokalny).
    let mut calendar_events = calendar::load_events();
    let mut calendar_stamp = calendar::stamp();
    let mut holiday_country = calendar::preferred_country();
    // Aktywny minutnik/stoper zapisany przez `luna-clock` — widget pod paskiem.
    let mut clock_activity: Option<ClockActivity> = ClockActivity::load();
    let mut clock_activity_stamp = clock_state::stamp();
    let mut clock_widget_hover = false;
    let mut notified_timer_end: Option<i64> = None;
    // Harmonogram motywu z `luna-clock` działa również przy zamkniętej aplikacji.
    let mut theme_schedule = ThemeSchedule::load();
    let mut theme_schedule_stamp = ThemeSchedule::stamp();

    // Dźwięk
    let mut volume_pct: u8 = 80;
    let mut volume_muted = false;

    // Wi-Fi (lista sieci pobierana w tle — `nmcli` potrafi trwać sekundy).
    let wifi_scan = luna::network::WifiScan::spawn();

    let mut osd = Osd::new(IconName::Volume, libluna_i18n::tr("Volume"), 0.0);
    let mut notes = Notifications::new();
    notes.set_placement(load_placement());
    // Powiadomienia, których użytkownik jeszcze nie widział w centrum (kropka przy zegarze).
    let mut unseen = 0usize;
    let mut icon_theme = IconSettings::load().theme;
    let mut placement_check = Instant::now();

    // Daemon powiadomień D‑Bus — rejestrowany **w tle**, żeby pierwsza klatka panelu nie
    // czekała na magistralę (zbus potrafi blokować, gdy bus dopiero wstaje po zalogowaniu).
    let bus_slot: BusSlot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let bus_ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut bus_started = false;
    let mut bus_retry = Instant::now();
    // System tray (StatusNotifierItem) — watcher/host rejestrujemy w tle, żeby
    // start panelu nie czekał na D-Bus.
    let tray_slot: TraySlot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let tray_ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut tray_started = false;

    let mut brightness_pct: u8 = 0;
    let mut brightness_state: Option<u8> = None;
    let mut battery_state: Option<luna::system::BatteryStatus> = None;
    let mut bluetooth_state: Option<(bool, usize)> = None;
    let mut audio_sink: Option<String> = None;
    // Tryb „nie przeszkadzać” — trzymany w pliku, żeby przetrwał przelogowanie.
    let mut dnd = luna::system::dnd();
    // Night light (gamma LUT) i oszczędzanie energii.
    let mut night_light = luna::system::night_light_percent();
    let mut night_available = false;
    let mut night_checked: Option<Instant> = None;
    let mut night_sent = false;
    let mut power_saver = luna::system::power_saver();
    let mut power_sent = false;
    // Odtwarzacz MPRIS (tytuł/wykonawca w Quick Panelu).
    let media = luna::media::Media::spawn();
    // Lista okien: tworzymy ją w pierwszej klatce, gdy mamy już `waker` — zmiana
    // listy okien ma natychmiast odświeżyć panel (inaczej przycisk programu pojawiał
    // się dopiero przy zmianie zegara).
    let mut toplevels: Option<luna::toplevels::Toplevels> = None;
    let workspaces = luna::workspaces::Workspaces::spawn();
    let system = luna::system::spawn();
    let volume = luna::volume::VolumeWatcher::spawn();
    // Systemowy motyw (portal) → plik dla wszystkich procesów Luny (tryb Auto).
    luna::system::watch_system_theme();

    // Sygnatura klatki (typ wnioskowany z przypisania `last_sig = sig` niżej).
    let mut last_sig = Default::default();
    let mut vol_seq: Option<u64> = None;
    let mut last_ws: Option<usize> = None;
    let mut last_media: Option<luna::media::MediaInfo> = None;

    // Diagnostyka wydajności (`LUNA_STATS=1`): ile klatek na sekundę robi powłoka i ile
    // czasu spędza w swoim kodzie (budowa sceny + trafienia). Pozwala odróżnić koszt
    // powłoki od kosztu kompozytora i page-flipu.
    let stats = std::env::var("LUNA_STATS").is_ok();
    // Przez pierwsze sekundy po starcie budzimy powłokę częściej — kompozytor DRM
    // może jeszcze dopisywać flagę gamma, a zapisany night light trzeba zastosować.
    let startup = Instant::now();
    if stats {
        eprintln!(
            "luna-shell: profil={} (debug_assertions={}), wysokość chrome={} px",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            cfg!(debug_assertions),
            CHROME_H
        );
    }
    let mut stat_frames = 0u64;
    let mut stat_ms = 0.0f64;
    let mut stat_report = Instant::now();

    run(cfg, move |frame: &mut Frame| {
        if toplevels.is_none() {
            toplevels = luna::toplevels::Toplevels::spawn(Some(frame.waker()));
        }
        let dt = frame.delta_time();
        let t_frame = Instant::now();

        // Stan bezpieczeństwa czytamy najwyżej co 500 ms; plik jest mały, a mtime
        // pozwala pominąć odświeżanie, gdy daemon nic nie zmienił.
        if Instant::now() >= security_check {
            security_check = Instant::now() + std::time::Duration::from_millis(500);
            let stamp = luna::security::state_stamp();
            if stamp != security_stamp {
                security_stamp = stamp;
                security_snapshot = luna::security::load_snapshot();
                if security_snapshot.prompts.is_empty() {
                    security_hover = None;
                }
            }
        }
        let security_modal = !security_snapshot.prompts.is_empty();
        state.security = panel::SecurityIndicators {
            microphone: security_snapshot.active(luna::security::Capability::Microphone),
            camera: security_snapshot.active(luna::security::Capability::Camera),
            network: security_snapshot
                .prompts
                .iter()
                .any(|p| p.capability == luna::security::Capability::Network)
                || security_snapshot.activities.iter().any(|a| {
                    a.caps.contains(&luna::security::Capability::Network)
                        && a.status != luna::security::Status::Allowed
                }),
            alert: security_snapshot.alert(),
        };

        // Podświetlenie pozycji pod kursorem w launcherze/popupie wymaga reakcji na
        // ruch myszy. Włączamy to tylko, gdy coś jest otwarte — w spoczynku ruch
        // kursora nie wywołuje wtedy żadnych commitów bufora (patrz `libluna-platform`).
        frame.set_motion_redraw(
            (animating_popup != ActivePopup::None && target_progress > 0.0)
                || !security_snapshot.prompts.is_empty(),
        );

        if !bus_started {
            bus_started = true;
            let slot = std::sync::Arc::clone(&bus_slot);
            let ready = std::sync::Arc::clone(&bus_ready);
            let waker = frame.waker();
            std::thread::spawn(move || {
                let bus = NotificationsBus::spawn(waker);
                if let Ok(mut g) = slot.lock() {
                    *g = bus;
                }
                ready.store(true, std::sync::atomic::Ordering::Relaxed);
            });
        }
        // Nazwa mogła zostać zwolniona przez stary proces albo magistrala wstała
        // później niż powłoka — ponawiamy rejestrację co kilka sekund.
        if bus_ready.load(std::sync::atomic::Ordering::Relaxed) {
            let missing = bus_slot
                .lock()
                .map(|slot| slot.as_ref().map(|bus| !bus.is_connected()).unwrap_or(true))
                .unwrap_or(true);
            if missing && Instant::now() >= bus_retry {
                bus_retry = Instant::now() + std::time::Duration::from_secs(5);
                bus_ready.store(false, std::sync::atomic::Ordering::Relaxed);
                let slot = std::sync::Arc::clone(&bus_slot);
                let ready = std::sync::Arc::clone(&bus_ready);
                let waker = frame.waker();
                std::thread::spawn(move || {
                    let bus = NotificationsBus::spawn(waker);
                    if let Ok(mut g) = slot.lock() {
                        *g = bus;
                    }
                    ready.store(true, std::sync::atomic::Ordering::Relaxed);
                });
            }
        }
        if !tray_started {
            tray_started = true;
            let slot = std::sync::Arc::clone(&tray_slot);
            let ready = std::sync::Arc::clone(&tray_ready);
            let waker = frame.waker();
            std::thread::spawn(move || {
                let bus = luna::tray::TrayBus::spawn(waker);
                if let Ok(mut g) = slot.lock() {
                    *g = bus;
                }
                ready.store(true, std::sync::atomic::Ordering::Relaxed);
            });
        }
        if tray_ready.load(std::sync::atomic::Ordering::Relaxed) {
            with_tray(&tray_slot, |b| state.tray = b.items());
        }
        if bus_ready.load(std::sync::atomic::Ordering::Relaxed) {
            with_notifications(&bus_slot, |b| {
                for inc in b.drain() {
                    if dnd {
                        // Tryb „nie przeszkadzać”: nie pokazujemy toasta, ale wpis
                        // zostaje w centrum powiadomień (klik w zegar).
                        let mut n = notification_from(inc);
                        let id = n.id;
                        // Sygnał zamknięcia leci od razu — wpis nie jest już „aktywny”.
                        n.closed = true;
                        notes.record(n);
                        unseen += 1;
                        b.signal_closed(id, 2);
                        if stats {
                            eprintln!("luna-shell: powiadomienie pominięte (DND)");
                        }
                    } else {
                        if stats {
                            eprintln!(
                                "luna-shell: powiadomienie [{}]: {}",
                                inc.app_name, inc.summary
                            );
                        }
                        notes.push(notification_from(inc));
                        unseen += 1;
                    }
                }
                // `CloseNotification` od aplikacji: usuwamy toast i wpis historii,
                // a nadawca dostaje `NotificationClosed` z powodem 3.
                for id in b.drain_closed() {
                    notes.remove_by_id(id);
                    b.signal_closed(id, 3);
                    if stats {
                        eprintln!("luna-shell: CloseNotification {id}");
                    }
                }
            });
        }

        let size = frame.size;
        let theme = &frame.theme;
        let text = frame.text;
        let bar = Rect::from_xywh(0.0, 0.0, size.width, THICKNESS);

        // Język mógł zmienić się w Ustawieniach — odbuduj etykiety, które są cachowane.
        let language = libluna_i18n::locale().language().to_owned();
        if language != ui_language {
            ui_language = language;
            // Nazwy aplikacji pochodzą z `Name[lang]` w plikach `.desktop` —
            // po zmianie języka trzeba je wczytać ponownie.
            if reload_apps(&mut apps, &mut launcher, true) {
                apps_stamp = luna::apps::stamp();
                app_menu = None;
                app_menu_hover = None;
            }
        }

        // Instalacja pakietu w Sklepie dopisuje plik `.desktop` — porównujemy tani
        // znacznik katalogów i przebudowujemy listę bez restartu powłoki. Sprawdzamy
        // najwyżej raz na sekundę, żeby nie statować setek plików co klatkę.
        if Instant::now() >= apps_checked {
            apps_checked = Instant::now() + std::time::Duration::from_secs(1);
            let stamp = luna::apps::stamp();
            if stamp != apps_stamp {
                apps_stamp = stamp;
                if reload_apps(&mut apps, &mut launcher, false) {
                    app_menu = None;
                    app_menu_hover = None;
                    if stats {
                        eprintln!(
                            "luna-shell: odświeżono listę aplikacji ({} pozycji)",
                            apps.len()
                        );
                    }
                }
            }
        }

        // Zegar i data
        let (clock, date, today_year, today_month, today_day) = now_clock_full();
        state.clock = clock;
        state.date = date;

        // Aplikacja `luna-clock` mogła dodać/zmienić wydarzenia — odśwież plan dnia.
        let events_stamp = calendar::stamp();
        if events_stamp != calendar_stamp {
            calendar_stamp = events_stamp;
            calendar_events = calendar::load_events();
        }

        let activity_stamp = clock_state::stamp();
        if activity_stamp != clock_activity_stamp {
            clock_activity_stamp = activity_stamp;
            clock_activity = ClockActivity::load();
        }
        let clock_now = clock_state::now_unix();
        let clock_widget_rect = if animating_popup == ActivePopup::None || target_progress <= 0.0 {
            clock_activity
                .as_ref()
                .and_then(|activity| clock_widget::visible_rect(frame.size, activity, clock_now))
        } else {
            None
        };

        // Minutnik zamkniętej aplikacji właśnie się skończył — pokaż lokalne powiadomienie.
        // Zakończony minutnik znika z widgetu (trafia do historii powiadomień).
        if clock_activity
            .as_ref()
            .is_some_and(|activity| !activity.is_app_running())
        {
            if let Some(timer) = clock_activity.as_ref().and_then(|a| a.timer.as_ref()) {
                if timer.running
                    && timer.end_unix <= clock_now
                    && notified_timer_end != Some(timer.end_unix)
                {
                    let notification =
                        Notification::new("Luna Clock", libluna_i18n::tr("Timer finished"), "")
                            .with_icon(IconName::Clock);
                    if dnd {
                        notes.record(notification);
                    } else {
                        notes.push(notification);
                    }
                    notified_timer_end = Some(timer.end_unix);
                }
            }

            // Zakończony minutnik znika także ze stanu widgetu — zostaje w powiadomieniach.
            if let Some(activity) = clock_activity.as_mut() {
                if activity
                    .timer
                    .as_ref()
                    .is_some_and(|timer| timer.is_finished(clock_now))
                {
                    activity.timer = None;
                    let _ = activity.save();
                }
            }
        }

        let schedule_stamp = ThemeSchedule::stamp();
        if schedule_stamp != theme_schedule_stamp {
            theme_schedule_stamp = schedule_stamp;
            theme_schedule = ThemeSchedule::load();
            // Ten sam `clock.conf` wybiera kraj świąt w panelu.
            holiday_country = calendar::preferred_country();
        }
        theme_schedule.apply(luna::theme_schedule::current_hour());

        state.title.clear();
        if let Some(tl) = &toplevels {
            let tasks = tl.tasks();
            if stats && tasks.len() != state.tasks.len() {
                eprintln!(
                    "luna-shell: taskbar: {} okien: {}",
                    tasks.len(),
                    tasks
                        .iter()
                        .map(|t| t.title.as_str())
                        .collect::<Vec<_>>()
                        .join(" | ")
                );
            }
            state.tasks = tasks;
        } else {
            state.tasks.clear();
        }

        // Stan systemu
        if let Ok(sys) = system.lock() {
            state.user = sys.user.clone();
            state.toggles = build_toggles(&sys);
            brightness_pct = sys.brightness.unwrap_or(0);
            brightness_state = sys.brightness;
            battery_state = sys.battery;
            bluetooth_state = sys.bluetooth;
            audio_sink = sys.audio_sink.clone();
            if let Some((v, m)) = sys.volume {
                volume_pct = v;
                volume_muted = m;
            }
            if let Some(profile) = &sys.power_profile {
                power_saver = profile == "power-saver";
            }
        }
        // Jednorazowe zastosowanie zapisanego trybu oszczędzania. Preferujemy
        // `power-profiles-daemon`; bez niego ograniczamy tempo prezentacji kompozytora.
        if !power_sent {
            power_sent = true;
            if luna::system::ppd_available() {
                if power_saver {
                    luna::system::set_power_profile("power-saver");
                }
            } else if power_saver {
                control_send("power-saver 1");
            }
        }
        // Dostępność night lightu ogłasza kompozytor plikiem `luna-gamma`.
        if night_checked.map_or(true, |t| t.elapsed() >= std::time::Duration::from_secs(1)) {
            night_checked = Some(Instant::now());
            night_available = std::env::var_os("XDG_RUNTIME_DIR")
                .map(|rt| std::path::Path::new(&rt).join("luna-gamma"))
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|s| s.trim() == "1")
                .unwrap_or(false);
            if night_available && !night_sent {
                night_sent = true;
                if stats {
                    eprintln!("luna-shell: night light → {night_light}%");
                }
                control_send(&format!("night-light {night_light}"));
            }
        }
        // Lista sieci z wątku skanującego jest czytana bezpośrednio przy rysowaniu
        // i obsłudze kliknięć (świeża po `refresh()` przy otwarciu popupu).

        // Przestrzenie robocze
        match &workspaces {
            Some(ws) => {
                let list = ws.list();
                state.workspaces = list.len();
                state.current_ws = list.iter().position(|w| w.active).unwrap_or(0);
            }
            None => {
                state.workspaces = 0;
                state.current_ws = 0;
            }
        }

        // OSD głośności
        let (vseq, vvol) = volume.current();
        if let (Some(prev), Some((pct, muted))) = (vol_seq, vvol) {
            if vseq != prev {
                volume_pct = pct;
                volume_muted = muted;
                osd.icon = IconName::Volume;
                osd.show(luna::volume::label(muted), pct as f64 / 100.0);
            }
        }
        vol_seq = Some(vseq);

        // OSD przestrzeni roboczych
        if state.workspaces > 0 {
            if let Some(prev) = last_ws {
                if prev != state.current_ws {
                    osd.icon = IconName::Grid;
                    let frac = (state.current_ws + 1) as f64 / state.workspaces as f64;
                    osd.show(
                        libluna_i18n::tr_args(
                            "Workspace {n}",
                            &[("n", &(state.current_ws + 1).to_string())],
                        ),
                        frac,
                    );
                }
            }
            last_ws = Some(state.current_ws);
        }

        // Otwieranie/zamykanie **bez animacji wysuwania**: przy software'owym
        // kompozytorze „slide” wyglądał jak szarpanie (popup przesuwał się o 28 px
        // bez zanikania, po czym nagle znikał). Stan jest teraz natychmiastowy;
        // `anim_progress` zostaje jako stan 0/1 dla rysowania poniżej.
        let _ = anim_progress;
        anim_progress = target_progress;
        if anim_progress == 0.0 {
            animating_popup = ActivePopup::None;
        }

        let lw = LAUNCHER_W.min((size.width - 80.0).max(320.0));
        let larea = Rect::from_xywh((size.width - lw) / 2.0, THICKNESS + 24.0, lw, LAUNCHER_H);

        // Prostokąty przycisków paska
        let burger = panel::burger_rect(bar, &frame.theme);
        let clock_r = panel::clock_rect(bar, &frame.theme, text, &state);

        // Tekst do schowka z menu aplikacji — kopiujemy **po** zdarzeniach, żeby nie
        // kolidować z pożyczonym motywem/sceną bieżącej klatki.
        let mut clipboard_pending: Option<String> = None;

        // ── Obsługa zdarzeń wejścia ──
        for ev in frame.events {
            hover.handle_event(ev);
            match ev {
                // Kursor opuścił powierzchnię — czyścimy też hover listy aplikacji.
                Event::PointerLeave => {
                    launcher.hover = None;
                    app_menu_hover = None;
                    security_hover = None;
                    clock_widget_hover = false;
                }
                // ESC zamyka najpierw modal bezpieczeństwa (odmowa), potem menu/launcher.
                Event::Key(ke) if ke.state == ButtonState::Pressed && ke.keysym == 0xff1b => {
                    if let Some(prompt) = security_snapshot.prompts.first() {
                        // Zamknięcie modalna = bezpieczna odmowa; decyzja zostaje zapisana.
                        let _ = luna::security::decide(&prompt.id, false);
                        security_hover = None;
                    } else if app_menu.is_some() {
                        app_menu = None;
                        app_menu_hover = None;
                    } else if animating_popup == ActivePopup::Launcher
                        && !launcher.search.is_empty()
                    {
                        launcher.clear_search();
                    } else if animating_popup != ActivePopup::None {
                        target_progress = 0.0;
                    }
                }

                // Gdy wisi modal zgody, klawiatura nie może sterować launcherem pod spodem.
                Event::Key(ke)
                    if ke.state == ButtonState::Pressed
                        && !security_snapshot.prompts.is_empty() =>
                {
                    let _ = ke;
                }

                // Klawiatura w menu kontekstowym aplikacji: strzałki wybierają
                // pozycję, Enter ją uruchamia, Esc (wyżej) zamyka menu.
                Event::Key(ke)
                    if ke.state == ButtonState::Pressed
                        && app_menu.is_some()
                        && target_progress > 0.0 =>
                {
                    match ke.keysym {
                        0xff52 | 0xff54 => {
                            let step = if ke.keysym == 0xff52 { -1 } else { 1 };
                            if let Some(menu) = &app_menu {
                                app_menu_hover = app_menu_step(&menu.rows, app_menu_hover, step);
                            }
                        }
                        0xff0d | 0xff8d => {
                            if let (Some(menu), Some(row)) = (&app_menu, app_menu_hover) {
                                if let (Some(action), Some(app)) = (
                                    menu.rows.get(row).and_then(|r| r.action),
                                    apps.get(menu.app),
                                ) {
                                    match perform_app_menu(action, app, &mut launcher) {
                                        AppMenuEffect::CopyCommand(text) => {
                                            clipboard_pending = Some(text)
                                        }
                                        AppMenuEffect::CloseLauncher => target_progress = 0.0,
                                        AppMenuEffect::None => {}
                                    }
                                }
                            }
                            app_menu = None;
                            app_menu_hover = None;
                        }
                        // Każdy inny klawisz zamyka menu i nie wpada do wyszukiwania.
                        _ => {
                            app_menu = None;
                            app_menu_hover = None;
                        }
                    }
                }

                // Nawigacja klawiaturą i wyszukiwanie w launcherze
                Event::Key(ke)
                    if ke.state == ButtonState::Pressed
                        && animating_popup == ActivePopup::Launcher
                        && target_progress > 0.0 =>
                {
                    match ke.keysym {
                        0xff08 => {
                            // BackSpace — usuwa znak z pola wyszukiwania
                            launcher.search.pop();
                            launcher.scroll = 0.0;
                            launcher.selected = Some(0);
                            launcher.hover = None;
                        }
                        0xff52 | 0xff54 => {
                            let dy = if ke.keysym == 0xff52 { -1 } else { 1 };
                            let count = launcher.filter_apps(&apps).len();
                            launcher.navigate(count, 0, dy);
                            panel::ensure_selection_visible_count(
                                count,
                                &mut launcher,
                                larea,
                                &frame.theme,
                                text,
                            );
                        }
                        0xff51 | 0xff53 => {
                            let dx = if ke.keysym == 0xff51 { -1 } else { 1 };
                            let count = launcher.filter_apps(&apps).len();
                            launcher.navigate(count, dx, 0);
                            panel::ensure_selection_visible_count(
                                count,
                                &mut launcher,
                                larea,
                                &frame.theme,
                                text,
                            );
                        }
                        0xff0d | 0xff8d => {
                            let filtered = launcher.filter_apps(&apps);
                            if let Some(i) = launcher.selected_index(filtered.len()) {
                                if let Some(app) = filtered.get(i) {
                                    launch_from_launcher(&mut launcher, app);
                                }
                                target_progress = 0.0;
                            }
                        }
                        _ => {
                            // Najpierw **tekst z xkb** (obsługuje Shift, AltGr, martwe
                            // klawisze i kompozycję): „ ”, —, €, ą… Wcześniejsze mapowanie
                            // keysym→znak znało tylko ASCII/Latin-1/Extended-A, więc np.
                            // cudzysłowy drukarskie (keysym 0x0ad2) były po cichu gubione.
                            if ke.modifiers.logo || ke.modifiers.ctrl {
                                continue;
                            }
                            let mut typed = false;
                            if let Some(text) = &ke.text {
                                for c in text.chars().filter(|c| !c.is_control()) {
                                    launcher.search.push(c);
                                    typed = true;
                                }
                            } else if let Some(c) = keysym_to_char(ke.keysym) {
                                launcher.search.push(c);
                                typed = true;
                            }
                            if typed {
                                launcher.scroll = 0.0;
                                launcher.selected = Some(0);
                                launcher.hover = None;
                                if stats {
                                    eprintln!("luna-shell: szukaj: {:?}", launcher.search);
                                }
                            }
                        }
                    }
                }

                // 1) Przewijanie listy lub kart w launcherze.
                // `se.dy` jest w jednostkach powierzchni (jeden ząbek kółka = `SCROLL_UNIT`).
                // Lista/siatka przewija się płynnie w pikselach; karuzela skacze o kartę.
                Event::Scroll(se) => {
                    // Modal zgody blokuje przewijanie treści pod spodem.
                    if !security_snapshot.prompts.is_empty() {
                        let _ = se;
                        continue;
                    }
                    // Przewinięcie listy zmienia położenie kafli — menu PPM traci kotwicę.
                    if app_menu.is_some() {
                        app_menu = None;
                        app_menu_hover = None;
                    }
                    let notches = se.dy / SCROLL_UNIT;
                    let vertical = se.dy.abs() >= se.dx.abs();
                    let dir = if vertical { se.dy } else { se.dx };
                    if animating_popup == ActivePopup::Launcher && target_progress > 0.0 {
                        if larea.contains(se.position) {
                            let filtered_len = launcher.filter_apps(&apps).len();
                            if launcher.view == panel::LauncherView::Carousel {
                                let steps = (notches.round() as i64).clamp(-3, 3);
                                if steps != 0 {
                                    // W dół (dodatnie) → następna karta.
                                    launcher.navigate(filtered_len, steps, 0);
                                }
                            } else {
                                let max = panel::launcher_scroll_max_count(
                                    filtered_len,
                                    &launcher,
                                    larea,
                                    &frame.theme,
                                    text,
                                );
                                // Kółko w górę (dy < 0) → przewijamy listę w górę.
                                launcher.scroll_by_notches(notches, LAUNCHER_SCROLL_PER_NOTCH, max);
                            }
                            // Po przewinięciu zawartość pod kursorem mogła się zmienić.
                            launcher.hover = hover.position().and_then(|p| {
                                if larea.contains(p) {
                                    panel::launcher_app_at(
                                        &apps,
                                        &launcher,
                                        larea,
                                        &frame.theme,
                                        text,
                                        p,
                                    )
                                } else {
                                    None
                                }
                            });
                        }
                    } else if animating_popup == ActivePopup::Calendar && target_progress > 0.0 {
                        // 2) Kółko nad kalendarzem zmienia miesiąc (w górę = następny).
                        let crect = panel::calendar_popup_rect(bar, &frame.theme);
                        if crect.contains(se.position) && dir != 0.0 {
                            // W dół (dodatnie) → następny miesiąc.
                            if dir > 0.0 {
                                if cal_month == 12 {
                                    cal_month = 1;
                                    cal_year += 1;
                                } else {
                                    cal_month += 1;
                                }
                            } else {
                                if cal_month == 1 {
                                    cal_month = 12;
                                    cal_year -= 1;
                                } else {
                                    cal_month -= 1;
                                }
                            }
                        }
                    } else {
                        // 3) Kółko nad pigułką głośności na pasku reguluje dźwięk
                        let toggles = panel::toggle_rects(bar, &frame.theme, text, &state);
                        let vol_idx = state
                            .toggles
                            .iter()
                            .position(|t| t.action == ToggleAction::Volume);
                        let is_vol_pill = vol_idx
                            .and_then(|i| toggles.get(i))
                            .map(|r| r.contains(se.position))
                            .unwrap_or(false);
                        if is_vol_pill && dir != 0.0 {
                            // W górę (ujemne) → głośniej; w dół → ciszej.
                            let delta = if dir < 0.0 { 5 } else { -5 };
                            volume_pct = luna::system::adjust_volume(volume_pct, delta);
                        }

                        // 4) Kółko nad kropkami przestrzeni roboczych przełącza pulpit
                        let ws_rects = panel::workspace_rects(bar, &frame.theme, text, &state);
                        let is_ws_area = ws_rects.iter().any(|r| r.contains(se.position));
                        if is_ws_area && dir != 0.0 {
                            if let Some(ws) = &workspaces {
                                // W dół (dodatnie) → następna przestrzeń.
                                if dir > 0.0 && state.current_ws + 1 < state.workspaces {
                                    ws.activate(state.current_ws + 1);
                                } else if dir < 0.0 && state.current_ws > 0 {
                                    ws.activate(state.current_ws - 1);
                                }
                            }
                        }
                    }
                }

                // Modal zgody: podświetlenie przycisków.
                Event::PointerMotion { position, .. } if !security_snapshot.prompts.is_empty() => {
                    let layout = panel::security_dialog_layout(size, &frame.theme);
                    security_hover = panel::security_dialog_hit(layout, *position);
                }

                // Ruch kursora nad menu aplikacji — podświetlamy pozycję.
                Event::PointerMotion { position, .. } if app_menu.is_some() => {
                    if let Some(menu) = &app_menu {
                        app_menu_hover = panel::app_menu_item_at(menu.rect, &menu.rows, *position);
                    }
                }

                // Ruch kursora nad launcherem — podświetlamy pozycję pod wskaźnikiem.
                // Hover jest **niezależny** od zaznaczenia klawiatury (`selected`), więc
                // strzałki nie przeskakują, gdy tylko przesuniemy mysz.
                Event::PointerMotion { position, .. }
                    if animating_popup == ActivePopup::Launcher && target_progress > 0.0 =>
                {
                    launcher.hover = if larea.contains(*position) {
                        panel::launcher_app_at(
                            &apps,
                            &launcher,
                            larea,
                            &frame.theme,
                            text,
                            *position,
                        )
                    } else {
                        None
                    };
                }

                // Podświetlenie pozycji menu kontekstowego taskbara.
                Event::PointerMotion { position, .. }
                    if animating_popup == ActivePopup::TaskMenu && target_progress > 0.0 =>
                {
                    if let Some(menu) = &task_menu {
                        task_menu_hover =
                            panel::task_menu_item_at(menu.rect, menu.labels.len(), *position);
                    }
                }

                // Podświetlenie wiersza w centrum powiadomień (obok kalendarza).
                Event::PointerMotion { position, .. }
                    if animating_popup == ActivePopup::Calendar && target_progress > 0.0 =>
                {
                    let nrect =
                        panel::notification_center_rect(bar, &frame.theme, notes.history_len());
                    notify_hover = match panel::notification_center_hit(
                        nrect,
                        &frame.theme,
                        notes.history_len(),
                        *position,
                    ) {
                        Some(panel::NotificationHit::Row(row)) => Some(row),
                        _ => None,
                    };
                }

                // Modal zgody bezpieczeństwa ma pierwszeństwo przed resztą wejścia.
                Event::PointerButton(pe)
                    if pe.state == ButtonState::Pressed
                        && pe.button == PointerButton::Left
                        && !security_snapshot.prompts.is_empty() =>
                {
                    let layout = panel::security_dialog_layout(size, &frame.theme);
                    if let (Some(choice), Some(prompt)) = (
                        panel::security_dialog_hit(layout, pe.position),
                        security_snapshot.prompts.first(),
                    ) {
                        let _ = luna::security::decide(
                            &prompt.id,
                            choice == panel::SecurityChoice::Allow,
                        );
                        security_hover = None;
                    }
                }

                // PPM/inne przyciski nie przebijają modalna zgody.
                Event::PointerButton(pe)
                    if pe.state == ButtonState::Pressed
                        && pe.button != PointerButton::Left
                        && !security_snapshot.prompts.is_empty() =>
                {
                    let _ = pe;
                }

                // PPM na aplikacji w launcherze — menu kontekstowe (ulubione, administrator,
                // Sklep/odinstalowanie, kopiowanie polecenia). Launcher zostaje otwarty.
                // **Musi być przed** ogólnym PPM taskbara (ten zamyka każdy popup).
                Event::PointerButton(pe)
                    if pe.state == ButtonState::Pressed
                        && pe.button == PointerButton::Right
                        && animating_popup == ActivePopup::Launcher
                        && target_progress > 0.0 =>
                {
                    if let Some(i) = panel::launcher_app_at(
                        &apps,
                        &launcher,
                        larea,
                        &frame.theme,
                        text,
                        pe.position,
                    ) {
                        if let Some(anchor) =
                            panel::launcher_app_rect(&apps, &launcher, larea, &frame.theme, text, i)
                        {
                            let rows = app_menu_rows(&apps[i], launcher.is_pinned(&apps[i]));
                            let bounds = Rect::from_xywh(0.0, 0.0, size.width, size.height);
                            let rect = panel::app_menu_rect(anchor, &rows, bounds, &frame.theme);
                            app_menu = Some(AppMenuState { app: i, rect, rows });
                            app_menu_hover = None;
                            if stats {
                                eprintln!("luna-shell: menu aplikacji {:?}", apps[i].name);
                            }
                        }
                    } else if larea.contains(pe.position) {
                        // PPM na pustym tle launchera tylko zamyka menu.
                        app_menu = None;
                        app_menu_hover = None;
                    } else {
                        // PPM poza launcherem zamyka menu i launcher.
                        app_menu = None;
                        app_menu_hover = None;
                        target_progress = 0.0;
                    }
                }

                // PPM na przycisku okna — menu kontekstowe (pokaż/zamknij/przenieś).
                Event::PointerButton(pe)
                    if pe.state == ButtonState::Pressed && pe.button == PointerButton::Right =>
                {
                    if let Some(hit) = panel::tray_at(bar, &frame.theme, text, &state, pe.position)
                    {
                        match hit {
                            panel::TrayHit::Overflow => {
                                animating_popup = ActivePopup::Tray;
                                target_progress = 1.0;
                            }
                            panel::TrayHit::Icon(i) => {
                                if let Some(item) = state.tray.get(i).cloned() {
                                    let anchor =
                                        panel::tray_layout(bar, &frame.theme, text, &state)
                                            .icons
                                            .iter()
                                            .find(|(_, index)| *index == i)
                                            .map(|(rect, _)| *rect)
                                            .unwrap_or_else(|| {
                                                Rect::from_xywh(
                                                    pe.position.x - 11.0,
                                                    bar.y(),
                                                    panel::TRAY_ICON,
                                                    THICKNESS,
                                                )
                                            });
                                    if let Some(rows) = tray_menu_for(&tray_slot, &item) {
                                        let rect =
                                            panel::tray_context_rect(anchor, &rows, &frame.theme);
                                        tray_context = Some(TrayContextState { item, rows, rect });
                                        animating_popup = ActivePopup::TrayContext;
                                        target_progress = 1.0;
                                    } else {
                                        // Aplikacja bez DBusMenu — próbujemy jej własne menu.
                                        with_tray(&tray_slot, |bus| {
                                            bus.context_menu(
                                                &item,
                                                pe.position.x as i32,
                                                pe.position.y as i32,
                                            )
                                        });
                                    }
                                }
                            }
                        }
                    } else {
                        let idx =
                            panel::task_at(bar, &frame.theme, text, &state.tasks, pe.position);
                        if let Some(i) = idx {
                            if let Some(task) = state.tasks.get(i) {
                                let first = if task.minimized {
                                    panel::TaskMenuAction::Show
                                } else {
                                    panel::TaskMenuAction::Minimize
                                };
                                let mut actions = vec![first, panel::TaskMenuAction::Close];
                                // Pozycje „przenieś” tylko gdy kompozytor zgłasza przestrzenie.
                                for ws in 0..state.workspaces {
                                    actions.push(panel::TaskMenuAction::MoveTo(ws));
                                }
                                let labels: Vec<String> = actions
                                    .iter()
                                    .map(|a| match a {
                                        panel::TaskMenuAction::Show => libluna_i18n::tr("Show"),
                                        panel::TaskMenuAction::Minimize => libluna_i18n::tr("Hide"),
                                        panel::TaskMenuAction::Close => libluna_i18n::tr("Close"),
                                        panel::TaskMenuAction::MoveTo(ws) => libluna_i18n::tr_args(
                                            "Move to workspace {n}",
                                            &[("n", &(ws + 1).to_string())],
                                        ),
                                    })
                                    .collect();
                                if stats {
                                    eprintln!(
                                        "luna-shell: task menu {:?} → {:?} {:?}",
                                        task.title, actions, labels
                                    );
                                }
                                let rects =
                                    panel::task_rects(bar, &frame.theme, text, &state.tasks);
                                let anchor = rects.get(i).copied().unwrap_or_else(|| {
                                    Rect::from_xywh(
                                        pe.position.x - 40.0,
                                        bar.y(),
                                        80.0,
                                        bar.height(),
                                    )
                                });
                                let rect =
                                    panel::task_menu_rect(anchor, bar, actions.len(), &frame.theme);
                                task_menu = Some(TaskMenuState {
                                    task: i,
                                    title: task.title.clone(),
                                    rect,
                                    labels,
                                    actions,
                                });
                                task_menu_hover = None;
                                animating_popup = ActivePopup::TaskMenu;
                                target_progress = 1.0;
                            }
                        } else {
                            task_menu = None;
                            task_menu_hover = None;
                            target_progress = 0.0;
                        }
                    }
                }

                // Dowolny inny przycisk (prawy/środkowy) zamyka otwarty popup/launcher.
                Event::PointerButton(pe)
                    if pe.state == ButtonState::Pressed
                        && pe.button != PointerButton::Left
                        && animating_popup != ActivePopup::None
                        && target_progress > 0.0 =>
                {
                    target_progress = 0.0;
                }

                Event::PointerMotion { position, .. } => {
                    clock_widget_hover =
                        clock_widget_rect.is_some_and(|rect| rect.contains(*position));
                }

                // Lewy przycisk myszy
                Event::PointerButton(pe)
                    if pe.state == ButtonState::Pressed && pe.button == PointerButton::Left =>
                {
                    let pos = pe.position;
                    // Widget aktywnego licznika otwiera aplikację na właściwej stronie.
                    if let Some(rect) = clock_widget_rect {
                        if rect.contains(pos) {
                            if let Some(activity) = &clock_activity {
                                open_clock_activity(activity, clock_now);
                            }
                            continue;
                        }
                    }
                    let in_bar = pos.y >= bar.y() && pos.y <= bar.bottom();
                    let is_burger = in_bar && pos.x >= bar.x() && pos.x <= burger.right() + 8.0;
                    let is_clock =
                        in_bar && pos.x >= clock_r.x() - 4.0 && pos.x <= clock_r.right() + 4.0;
                    let is_quick =
                        in_bar && panel::quick_button_rect(bar, theme, text, &state).contains(pos);

                    // Gdy któreś menu jest otwarte (lub animowane)
                    if animating_popup != ActivePopup::None && target_progress > 0.0 {
                        let mut handled = false;

                        match animating_popup {
                            ActivePopup::TaskMenu => {
                                if let Some(menu) = task_menu.take() {
                                    if let Some(k) =
                                        panel::task_menu_item_at(menu.rect, menu.labels.len(), pos)
                                    {
                                        match menu.actions.get(k) {
                                            Some(panel::TaskMenuAction::Show) => {
                                                if let Some(tl) = &toplevels {
                                                    tl.show(menu.task);
                                                }
                                            }
                                            Some(panel::TaskMenuAction::Minimize) => {
                                                if let Some(tl) = &toplevels {
                                                    tl.minimize(menu.task);
                                                }
                                            }
                                            Some(panel::TaskMenuAction::Close) => {
                                                if let Some(tl) = &toplevels {
                                                    tl.close(menu.task);
                                                }
                                            }
                                            Some(panel::TaskMenuAction::MoveTo(ws)) => {
                                                control_send(&format!(
                                                    "move-to-workspace {} {}",
                                                    ws,
                                                    one_line(&menu.title)
                                                ));
                                            }
                                            None => {}
                                        }
                                    }
                                }
                                task_menu_hover = None;
                                target_progress = 0.0;
                                handled = true;
                            }
                            ActivePopup::Tray => {
                                if let Some(index) =
                                    panel::tray_menu_hit(bar, &frame.theme, text, &state, pos)
                                {
                                    if let Some(item) = state.tray.get(index).cloned() {
                                        with_tray(&tray_slot, |bus| {
                                            bus.activate(&item, pos.x as i32, pos.y as i32)
                                        });
                                    }
                                    target_progress = 0.0;
                                } else if panel::tray_menu_rect(bar, &frame.theme, text, &state)
                                    .contains(pos)
                                {
                                    // Klik w puste miejsce karty overflow nie zamyka
                                    // panelu (spójnie z menu taskbara).
                                    handled = true;
                                }
                            }
                            ActivePopup::TrayContext => {
                                if let Some(ctx) = &tray_context {
                                    if let Some(row) =
                                        panel::tray_context_hit(ctx.rect, &ctx.rows, pos)
                                    {
                                        let item = ctx.item.clone();
                                        let id = ctx.rows[row].id;
                                        with_tray(&tray_slot, |bus| bus.menu_event(&item, id));
                                        target_progress = 0.0;
                                    } else if ctx.rect.contains(pos) {
                                        handled = true;
                                    }
                                }
                            }
                            ActivePopup::Quick => {
                                let qrect = panel::quick_panel_rect(bar, &frame.theme);
                                if let Some(hit) = panel::quick_hit(qrect, &frame.theme, pos) {
                                    if stats {
                                        eprintln!("luna-shell: quick {:?}", hit);
                                    }
                                    match hit {
                                        panel::QuickHit::Settings => {
                                            luna::power::perform(
                                                luna::power::PowerAction::Settings,
                                            );
                                            target_progress = 0.0;
                                        }
                                        panel::QuickHit::Theme => {
                                            let mut s = libluna_core::ThemeSettings::load();
                                            let i = ThemePreference::ALL
                                                .iter()
                                                .position(|p| *p == s.preference())
                                                .unwrap_or(2);
                                            s.set_preference(
                                                ThemePreference::ALL
                                                    [(i + 1) % ThemePreference::ALL.len()],
                                            );
                                            let _ = s.save();
                                        }
                                        panel::QuickHit::Wifi => {
                                            // Kafelek pokazuje tylko bieżącą sieć — kliknięcie
                                            // otwiera listę sieci (radio wyłączamy z popupu).
                                            wifi_scan.refresh();
                                            animating_popup = ActivePopup::Wifi;
                                            target_progress = 1.0;
                                        }
                                        panel::QuickHit::Bluetooth => {
                                            if let Some((on, _)) = bluetooth_state {
                                                luna::system::set_bluetooth(!on);
                                            }
                                        }
                                        panel::QuickHit::Airplane => {
                                            let wifi_on = state
                                                .toggles
                                                .iter()
                                                .find(|t| t.action == ToggleAction::Wifi)
                                                .map(|t| t.active)
                                                .unwrap_or(true);
                                            let bt_on = matches!(bluetooth_state, Some((true, _)));
                                            let airplane = !wifi_on && !bt_on;
                                            luna::system::set_airplane(!airplane);
                                        }
                                        panel::QuickHit::Dnd => {
                                            dnd = !dnd;
                                            luna::system::set_dnd(dnd);
                                            if dnd {
                                                // Chowamy widoczne toasty (zamykając je po
                                                // stronie nadawcy); historia zostaje w centrum.
                                                for n in notes.clear_toasts() {
                                                    if n.id != 0 {
                                                        with_notifications(&bus_slot, |b| {
                                                            b.signal_closed(n.id, 2)
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                        panel::QuickHit::NightLight => {
                                            if night_available {
                                                night_light = if night_light > 0 { 0 } else { 60 };
                                                luna::system::set_night_light_percent(night_light);
                                                control_send(&format!("night-light {night_light}"));
                                            }
                                        }
                                        panel::QuickHit::PowerSaver => {
                                            power_saver = !power_saver;
                                            luna::system::set_power_saver(power_saver);
                                            if luna::system::ppd_available() {
                                                luna::system::set_power_profile(if power_saver {
                                                    "power-saver"
                                                } else {
                                                    "balanced"
                                                });
                                            } else {
                                                control_send(&format!(
                                                    "power-saver {}",
                                                    u8::from(power_saver)
                                                ));
                                            }
                                        }
                                        panel::QuickHit::Mute => {
                                            luna::system::toggle_mute();
                                            volume_muted = !volume_muted;
                                        }
                                        panel::QuickHit::Volume(v) => {
                                            luna::system::set_volume(v);
                                            volume_pct = v;
                                        }
                                        panel::QuickHit::Brightness(v) => {
                                            // Suwak jest aktywny tylko przy realnym podświetleniu.
                                            if brightness_state.is_some() {
                                                luna::system::set_brightness(v);
                                                brightness_pct = v;
                                                brightness_state = Some(v);
                                            }
                                        }
                                        panel::QuickHit::MediaPrevious => {
                                            if let Some(m) = media.current() {
                                                luna::media::send(&m.player, "Previous");
                                            }
                                        }
                                        panel::QuickHit::MediaPlayPause => {
                                            if let Some(m) = media.current() {
                                                luna::media::send(&m.player, "PlayPause");
                                            }
                                        }
                                        panel::QuickHit::MediaNext => {
                                            if let Some(m) = media.current() {
                                                luna::media::send(&m.player, "Next");
                                            }
                                        }
                                        panel::QuickHit::Lock => {
                                            luna::power::perform(luna::power::PowerAction::Lock);
                                            target_progress = 0.0;
                                        }
                                        panel::QuickHit::Logout => {
                                            luna::power::perform(luna::power::PowerAction::Logout);
                                            target_progress = 0.0;
                                        }
                                        panel::QuickHit::Reboot => {
                                            luna::power::perform(luna::power::PowerAction::Reboot);
                                            target_progress = 0.0;
                                        }
                                        panel::QuickHit::PowerOff => {
                                            luna::power::perform(
                                                luna::power::PowerAction::PowerOff,
                                            );
                                            target_progress = 0.0;
                                        }
                                    }
                                    handled = true;
                                }
                            }
                            ActivePopup::Launcher => {
                                if let Some(menu) = app_menu.take() {
                                    app_menu_hover = None;
                                    let inside_menu = menu.rect.contains(pos);
                                    let mut clicked = false;
                                    if let Some(row) =
                                        panel::app_menu_item_at(menu.rect, &menu.rows, pos)
                                    {
                                        if let (Some(action), Some(app)) =
                                            (menu.rows[row].action, apps.get(menu.app))
                                        {
                                            clicked = true;
                                            match perform_app_menu(action, app, &mut launcher) {
                                                AppMenuEffect::CopyCommand(text) => {
                                                    clipboard_pending = Some(text)
                                                }
                                                AppMenuEffect::CloseLauncher => {
                                                    target_progress = 0.0
                                                }
                                                AppMenuEffect::None => {}
                                            }
                                        }
                                    }
                                    // Klik poza menu i poza launcherem zamyka całość.
                                    if !clicked && !inside_menu && !larea.contains(pos) {
                                        target_progress = 0.0;
                                    }
                                    handled = true;
                                } else if !launcher.search.is_empty()
                                    && panel::launcher_search_clear_at(
                                        larea,
                                        &frame.theme,
                                        text,
                                        pos,
                                    )
                                {
                                    launcher.clear_search();
                                    handled = true;
                                } else if let Some(v) =
                                    panel::launcher_view_at(larea, &frame.theme, text, pos)
                                {
                                    launcher.view = v;
                                    launcher.scroll = 0.0;
                                    launcher.hover = None;
                                    handled = true;
                                } else if let Some(i) = panel::launcher_app_at(
                                    &apps,
                                    &launcher,
                                    larea,
                                    &frame.theme,
                                    text,
                                    pos,
                                ) {
                                    launch_from_launcher(&mut launcher, &apps[i]);
                                    target_progress = 0.0;
                                    handled = true;
                                } else if larea.contains(pos) {
                                    // Kliknięcie we wnętrze launchera (puste tło) nie zamyka go
                                    handled = true;
                                }
                            }
                            ActivePopup::Wifi => {
                                let wrect = panel::wifi_popup_rect(bar, &frame.theme, text, &state);
                                if wrect.contains(pos) {
                                    let conn = state
                                        .toggles
                                        .iter()
                                        .find(|t| t.action == ToggleAction::Wifi && t.active)
                                        .map(|t| t.label.as_str());
                                    if let Some(act) = panel::wifi_popup_hit(
                                        wrect,
                                        &frame.theme,
                                        pos,
                                        conn,
                                        &wifi_scan.current(),
                                    ) {
                                        match act {
                                            WifiAction::TogglePower => {
                                                let on = state
                                                    .toggles
                                                    .iter()
                                                    .find(|t| t.action == ToggleAction::Wifi)
                                                    .map(|t| t.active)
                                                    .unwrap_or(true);
                                                luna::system::set_wifi(!on);
                                                wifi_scan.refresh();
                                            }
                                            WifiAction::SelectNetwork(ssid) => {
                                                let snapshot = wifi_scan.snapshot();
                                                if let Some(net) = snapshot
                                                    .wifi
                                                    .iter()
                                                    .find(|net| net.ssid == ssid)
                                                {
                                                    let device = net.device.clone();
                                                    let security = net.security;
                                                    let known = snapshot.is_known(&ssid);
                                                    if security.needs_password() && !known {
                                                        // Bez hasła w popupie — otwieramy Ustawienia.
                                                        open_network_settings();
                                                    } else {
                                                        let ssid = ssid.clone();
                                                        std::thread::spawn(move || {
                                                            if luna::network::connect_wifi(
                                                                &device, &ssid, security, None,
                                                                false,
                                                            )
                                                            .is_err()
                                                            {
                                                                open_network_settings();
                                                            }
                                                        });
                                                    }
                                                    wifi_scan.refresh();
                                                }
                                            }
                                        }
                                    }
                                    handled = true;
                                }
                            }
                            ActivePopup::Calendar => {
                                let crect = panel::calendar_popup_rect(bar, &frame.theme);
                                let nrect = panel::notification_center_rect(
                                    bar,
                                    &frame.theme,
                                    notes.history_len(),
                                );
                                if nrect.contains(pos) {
                                    match panel::notification_center_hit(
                                        nrect,
                                        &frame.theme,
                                        notes.history_len(),
                                        pos,
                                    ) {
                                        Some(panel::NotificationHit::ClearAll) => {
                                            let ids: Vec<u32> = notes
                                                .history()
                                                .iter()
                                                .filter(|n| !n.closed && n.id != 0)
                                                .map(|n| n.id)
                                                .collect();
                                            for id in ids {
                                                with_notifications(&bus_slot, |b| {
                                                    b.signal_closed(id, 2)
                                                });
                                            }
                                            notes.clear_history();
                                        }
                                        Some(panel::NotificationHit::Row(row)) => {
                                            let idx = notes.history_len().saturating_sub(1 + row);
                                            let mut open = None;
                                            if let Some(n) = notes.history_get(idx) {
                                                let id = n.id;
                                                if id != 0 && !n.closed {
                                                    with_notifications(&bus_slot, |b| {
                                                        b.signal_closed(id, 2)
                                                    });
                                                }
                                                open =
                                                    Some((n.desktop_entry.clone(), n.app.clone()));
                                            }
                                            if let Some((entry, name)) = open {
                                                open_notification_source(
                                                    entry.as_deref(),
                                                    &name,
                                                    toplevels.as_ref(),
                                                    &apps,
                                                );
                                            }
                                            notes.remove_history(idx);
                                        }
                                        None => {}
                                    }
                                    handled = true;
                                } else if crect.contains(pos) {
                                    if let Some(act) = panel::calendar_popup_hit(
                                        crect,
                                        &frame.theme,
                                        pos,
                                        cal_year,
                                        cal_month,
                                    ) {
                                        match act {
                                            CalendarAction::PrevMonth => {
                                                if cal_month == 1 {
                                                    cal_month = 12;
                                                    cal_year -= 1;
                                                } else {
                                                    cal_month -= 1;
                                                }
                                            }
                                            CalendarAction::NextMonth => {
                                                if cal_month == 12 {
                                                    cal_month = 1;
                                                    cal_year += 1;
                                                } else {
                                                    cal_month += 1;
                                                }
                                            }
                                            CalendarAction::SelectDay(d) => {
                                                cal_selected = Some(d);
                                            }
                                            CalendarAction::OpenApp => {
                                                let day = cal_selected.or_else(|| {
                                                    (cal_year == today_year
                                                        && cal_month == today_month)
                                                        .then_some(today_day)
                                                });
                                                open_calendar_app(cal_year, cal_month, day);
                                                target_progress = 0.0;
                                            }
                                        }
                                    }
                                    handled = true;
                                }
                            }
                            ActivePopup::None => {}
                        }

                        if !handled {
                            // Sprawdzamy, czy kliknięto przycisk na pasku (przełączenie lub zamknięcie)
                            if is_burger {
                                if animating_popup == ActivePopup::Launcher {
                                    target_progress = 0.0;
                                } else {
                                    animating_popup = ActivePopup::Launcher;
                                    target_progress = 1.0;
                                }
                            } else if is_clock {
                                if animating_popup == ActivePopup::Calendar {
                                    target_progress = 0.0;
                                } else {
                                    cal_year = today_year;
                                    cal_month = today_month;
                                    cal_selected = Some(today_day);
                                    animating_popup = ActivePopup::Calendar;
                                    target_progress = 1.0;
                                    // Otwarcie centrum = powiadomienia uznane za obejrzane.
                                    unseen = 0;
                                }
                            } else if is_quick {
                                if animating_popup == ActivePopup::Quick {
                                    target_progress = 0.0;
                                } else {
                                    animating_popup = ActivePopup::Quick;
                                    target_progress = 1.0;
                                }
                            } else if let Some(hit) =
                                panel::tray_at(bar, &frame.theme, text, &state, pos)
                            {
                                match hit {
                                    panel::TrayHit::Overflow => {
                                        animating_popup = ActivePopup::Tray;
                                        target_progress = 1.0;
                                    }
                                    panel::TrayHit::Icon(i) => {
                                        if let Some(item) = state.tray.get(i).cloned() {
                                            with_tray(&tray_slot, |bus| {
                                                bus.activate(&item, pos.x as i32, pos.y as i32)
                                            });
                                        }
                                    }
                                }
                            } else if let Some(i) =
                                panel::toggle_at(bar, &frame.theme, text, &state, pos)
                            {
                                match state.toggles[i].action {
                                    ToggleAction::Wifi => {
                                        wifi_scan.refresh();
                                        animating_popup = ActivePopup::Wifi;
                                        target_progress = 1.0;
                                    }
                                    ToggleAction::Volume => {
                                        animating_popup = ActivePopup::Quick;
                                        target_progress = 1.0;
                                    }
                                    ToggleAction::Brightness => {
                                        let next =
                                            luna::system::next_brightness_level(brightness_pct);
                                        luna::system::set_brightness(next);
                                        brightness_pct = next;
                                        brightness_state = Some(next);
                                        osd.icon = IconName::Brightness;
                                        osd.show(
                                            libluna_i18n::tr("Brightness"),
                                            next as f64 / 100.0,
                                        );
                                        target_progress = 0.0;
                                    }
                                    // Bateria i inne wskaźniki statusu również otwierają
                                    // wspólny panel sterowania.
                                    ToggleAction::None => {
                                        animating_popup = ActivePopup::Quick;
                                        target_progress = 1.0;
                                    }
                                }
                            } else if let Some(i) =
                                panel::task_at(bar, &frame.theme, text, &state.tasks, pos)
                            {
                                if let Some(tl) = &toplevels {
                                    tl.activate(i);
                                }
                                target_progress = 0.0;
                            } else if let Some(i) =
                                panel::workspace_at(bar, &frame.theme, text, &state, pos)
                            {
                                if let Some(ws) = &workspaces {
                                    ws.activate(i);
                                }
                                target_progress = 0.0;
                            } else {
                                // Kliknięcie gdziekolwiek poza otwartym popupem/launcherem — ZAMYKA GO!
                                target_progress = 0.0;
                            }
                        }
                    } else {
                        // Żaden popup nie jest otwarty — standardowa obsługa paska
                        if is_burger {
                            let max = panel::launcher_scroll_max(
                                &apps,
                                &launcher,
                                larea,
                                &frame.theme,
                                text,
                            );
                            launcher.scroll = launcher.scroll.clamp(0.0, max);
                            animating_popup = ActivePopup::Launcher;
                            target_progress = 1.0;
                        } else if is_clock {
                            cal_year = today_year;
                            cal_month = today_month;
                            cal_selected = Some(today_day);
                            animating_popup = ActivePopup::Calendar;
                            target_progress = 1.0;
                            unseen = 0;
                        } else if is_quick {
                            animating_popup = ActivePopup::Quick;
                            target_progress = 1.0;
                        } else if let Some(hit) =
                            panel::tray_at(bar, &frame.theme, text, &state, pos)
                        {
                            match hit {
                                panel::TrayHit::Overflow => {
                                    animating_popup = ActivePopup::Tray;
                                    target_progress = 1.0;
                                }
                                panel::TrayHit::Icon(i) => {
                                    if let Some(item) = state.tray.get(i).cloned() {
                                        with_tray(&tray_slot, |bus| {
                                            bus.activate(&item, pos.x as i32, pos.y as i32)
                                        });
                                    }
                                }
                            }
                        } else if let Some(i) =
                            panel::toggle_at(bar, &frame.theme, text, &state, pos)
                        {
                            match state.toggles[i].action {
                                ToggleAction::Wifi => {
                                    wifi_scan.refresh();
                                    animating_popup = ActivePopup::Wifi;
                                    target_progress = 1.0;
                                }
                                ToggleAction::Volume => {
                                    animating_popup = ActivePopup::Quick;
                                    target_progress = 1.0;
                                }
                                ToggleAction::Brightness => {
                                    let next = luna::system::next_brightness_level(brightness_pct);
                                    luna::system::set_brightness(next);
                                    brightness_pct = next;
                                    brightness_state = Some(next);
                                    osd.icon = IconName::Brightness;
                                    osd.show(libluna_i18n::tr("Brightness"), next as f64 / 100.0);
                                }
                                ToggleAction::None => {
                                    animating_popup = ActivePopup::Quick;
                                    target_progress = 1.0;
                                }
                            }
                        } else if let Some(i) =
                            panel::task_at(bar, &frame.theme, text, &state.tasks, pos)
                        {
                            if let Some(tl) = &toplevels {
                                tl.activate(i);
                            }
                        } else if let Some(i) =
                            panel::workspace_at(bar, &frame.theme, text, &state, pos)
                        {
                            if let Some(ws) = &workspaces {
                                ws.activate(i);
                            }
                        } else {
                            // Obsługa toastów powiadomień
                            let area = Rect::from_xywh(
                                0.0,
                                THICKNESS + 8.0,
                                size.width,
                                size.height - THICKNESS,
                            );
                            if let Some((item, ak)) = notes.action_at(area, pos, &frame.theme, text)
                            {
                                let mut open = None;
                                if let Some(n) = notes.get(item) {
                                    if let Some((key, _)) = n.actions.get(ak) {
                                        let key = key.clone();
                                        let id = n.id;
                                        if key == "default" {
                                            open = Some((n.desktop_entry.clone(), n.app.clone()));
                                        }
                                        with_notifications(&bus_slot, |b| {
                                            b.signal_action(id, &key);
                                            b.signal_closed(id, 2);
                                        });
                                    } else {
                                        let id = n.id;
                                        with_notifications(&bus_slot, |b| b.signal_closed(id, 2));
                                    }
                                }
                                if let Some((entry, name)) = open {
                                    open_notification_source(
                                        entry.as_deref(),
                                        &name,
                                        toplevels.as_ref(),
                                        &apps,
                                    );
                                }
                                notes.dismiss(item);
                            } else if let Some(i) = notes.hit(area, pos) {
                                let mut open = None;
                                if let Some(n) = notes.get(i) {
                                    let id = n.id;
                                    if n.desktop_entry.is_some() {
                                        // Luna ma wpis `.desktop` — otwieramy/aktywujemy
                                        // aplikację lokalnie (aplikacje Luny nieobsługują
                                        // `ActionInvoked`, więc sam sygnał by nie zadziałał).
                                        open = Some((n.desktop_entry.clone(), n.app.clone()));
                                        with_notifications(&bus_slot, |b| b.signal_closed(id, 2));
                                    } else if n.actions.iter().any(|(k, _)| k == "default") {
                                        with_notifications(&bus_slot, |b| {
                                            b.signal_action(id, "default");
                                            b.signal_closed(id, 2);
                                        });
                                    } else {
                                        with_notifications(&bus_slot, |b| b.signal_closed(id, 2));
                                    }
                                }
                                if let Some((entry, name)) = open {
                                    open_notification_source(
                                        entry.as_deref(),
                                        &name,
                                        toplevels.as_ref(),
                                        &apps,
                                    );
                                }
                                notes.dismiss(i);
                            }
                        }
                    }
                }

                // Środkowy klik na zadaniu w pasku — zamyka okno
                Event::PointerButton(pe)
                    if pe.state == ButtonState::Pressed && pe.button == PointerButton::Middle =>
                {
                    if let Some(panel::TrayHit::Icon(i)) =
                        panel::tray_at(bar, &frame.theme, text, &state, pe.position)
                    {
                        if let Some(item) = state.tray.get(i).cloned() {
                            with_tray(&tray_slot, |bus| {
                                bus.secondary_activate(
                                    &item,
                                    pe.position.x as i32,
                                    pe.position.y as i32,
                                )
                            });
                        }
                    } else if let Some(i) =
                        panel::task_at(bar, &frame.theme, text, &state.tasks, pe.position)
                    {
                        if let Some(tl) = &toplevels {
                            tl.close(i);
                        }
                    }
                }
                _ => {}
            }
        }

        // Launcher zapomina frazę po zamknięciu — każde otwarcie pokazuje pełną listę.
        let launcher_closed = animating_popup != ActivePopup::Launcher || target_progress <= 0.0;
        if launcher_closed {
            app_menu = None;
            app_menu_hover = None;
            launcher.hover = None;
            if !launcher.search.is_empty() {
                launcher.clear_search();
            }
        }

        // Otwarty launcher/popup przechwytuje **całą** powierzchnię panelu. Dzięki temu
        // kliknięcie poza obrysem (puste tło, inny monitor, dolna część ekranu) trafia
        // do powłoki i zamyka popup, zamiast aktywować okno pod spodem. Ustawiamy to
        // **po** obsłudze zdarzeń, żeby zmiana działała już w tej samej klatce.
        let popup_open =
            (animating_popup != ActivePopup::None && target_progress > 0.0) || security_modal;
        let clock_widget_rect_now = if popup_open {
            None
        } else {
            clock_activity
                .as_ref()
                .and_then(|activity| clock_widget::visible_rect(frame.size, activity, clock_now))
        };
        if clock_widget_rect_now.is_none() {
            clock_widget_hover = false;
        }
        if std::env::var("LUNA_STATS").is_ok() && animating_popup != logged_popup {
            eprintln!(
                "luna-shell: popup {:?} → {:?} (cel {:.0})",
                logged_popup, animating_popup, target_progress
            );
            logged_popup = animating_popup;
        }
        frame.set_input_region(if popup_open {
            InputRegion::Rects(vec![Rect::from_xywh(
                0.0,
                0.0,
                frame.size.width,
                frame.size.height,
            )])
        } else {
            // Powierzchnia powłoki jest wyższa niż pasek (mieści popupy), więc
            // region wejścia musi obejmować **tylko** pasek i ewentualne toasty.
            // Bez tego przezroczysty pas do 680 px pożerał kliknięcia okien pod nim
            // (nieklikalne menu Zed, przyciski tytułu Firefoksa itd.).
            let mut rects = vec![Rect::from_xywh(0.0, 0.0, frame.size.width, THICKNESS)];
            if let Some(widget) = clock_widget_rect_now {
                rects.push(widget);
            }
            if notes.visible() {
                let area = Rect::from_xywh(
                    0.0,
                    THICKNESS + 8.0,
                    frame.size.width,
                    frame.size.height - THICKNESS,
                );
                rects.push(notes.stack_rect(area));
            }
            InputRegion::Rects(rects)
        });

        osd.advance(dt);
        // Miejsce toastów i tryb „nie przeszkadzać” mogą zmienić Ustawienia;
        // sprawdzamy pliki raz na sekundę.
        if Instant::now() >= placement_check {
            let placement = load_placement();
            if placement != notes.placement() {
                notes.set_placement(placement);
            }
            let file_dnd = luna::system::dnd();
            if file_dnd != dnd {
                dnd = file_dnd;
                if dnd {
                    for n in notes.clear_toasts() {
                        if n.id != 0 {
                            with_notifications(&bus_slot, |b| b.signal_closed(n.id, 2));
                        }
                    }
                }
            }
            let icons = IconSettings::load();
            if icons.theme != icon_theme {
                icon_theme = icons.theme;
            }
            placement_check = Instant::now() + std::time::Duration::from_secs(1);
        }
        for expired in notes.advance(dt) {
            if expired.id != 0 {
                with_notifications(&bus_slot, |b| b.signal_closed(expired.id, 1));
            }
        }
        state.notifications = notes.history_len();
        if animating_popup == ActivePopup::Calendar && target_progress > 0.0 {
            // Otwarte centrum pokazuje nowe wpisy od razu — kropka nie jest potrzebna.
            unseen = 0;
        }
        state.unseen = unseen;
        if animating_popup != ActivePopup::Calendar || target_progress <= 0.0 {
            notify_hover = None;
        }

        // Sygnatura stanu — liczymy ją **przed** rysowaniem, żeby dało się pominąć całą
        // klatkę, gdy nic się nie zmieniło (np. sam ruch kursora nad tym samym kafelkiem).
        let mut th: u64 = state.tasks.len() as u64;
        for t in &state.tasks {
            th = th
                .wrapping_mul(1_099_511_628_211)
                .wrapping_add(u64::from(t.active) ^ t.title.len() as u64);
        }
        for tg in &state.toggles {
            let mut lh: u64 = 0;
            for b in tg.label.bytes() {
                lh = lh.wrapping_mul(31).wrapping_add(u64::from(b));
            }
            th = th
                .wrapping_mul(1_099_511_628_211)
                .wrapping_add(u64::from(tg.active) ^ lh);
        }
        th = th
            .wrapping_mul(1_099_511_628_211)
            .wrapping_add(state.user.len() as u64);
        th = th
            .wrapping_mul(1_099_511_628_211)
            .wrapping_add(state.workspaces as u64 ^ ((state.current_ws as u64) << 8));
        // Historia powiadomień zmienia się niezależnie od toastów.
        th = th
            .wrapping_mul(1_099_511_628_211)
            .wrapping_add(notes.history_len() as u64 ^ ((u64::from(dnd)) << 16));
        if let Some(n) = notes.history().last() {
            for b in n.title.bytes() {
                th = th.wrapping_mul(31).wrapping_add(u64::from(b));
            }
        }

        // Stan hoveru wchodzi do sygnatury klatki — bez tego szybka ścieżka
        // „tylko ruch kursora” pomijałaby podświetlenie nowego elementu.
        let hp = hover.position();
        let panel_hover = (
            hp.is_some_and(|p| burger.contains(p)),
            hp.is_some_and(|p| clock_r.contains(p)),
            hp.is_some_and(|p| panel::quick_button_rect(bar, theme, text, &state).contains(p)),
            hp.and_then(|p| panel::task_at(bar, theme, text, &state.tasks, p)),
            hp.and_then(|p| panel::workspace_at(bar, theme, text, &state, p)),
            hp.and_then(|p| panel::toggle_at(bar, theme, text, &state, p)),
            hp.and_then(|p| panel::tray_at(bar, theme, text, &state, p)),
        );
        let launcher_open = animating_popup == ActivePopup::Launcher && target_progress > 0.0;
        let launcher_view_hover = if launcher_open {
            hp.and_then(|p| {
                panel::launcher_view_at(larea, theme, text, p).map(|view| view as usize)
            })
        } else {
            None
        };
        let launcher_clear_hover = launcher_open
            && !launcher.search.is_empty()
            && hp.is_some_and(|p| panel::launcher_search_clear_at(larea, theme, text, p));
        let quick_hover: Option<panel::QuickHit> =
            if animating_popup == ActivePopup::Quick && target_progress > 0.0 {
                hp.and_then(|p| panel::quick_hit(panel::quick_panel_rect(bar, theme), theme, p))
            } else {
                None
            };
        let tray_hover: Option<usize> =
            if animating_popup == ActivePopup::Tray && target_progress > 0.0 {
                hp.and_then(|p| panel::tray_menu_hit(bar, theme, text, &state, p))
            } else {
                None
            };
        let tray_context_hover: Option<usize> =
            if animating_popup == ActivePopup::TrayContext && target_progress > 0.0 {
                tray_context.as_ref().and_then(|ctx| {
                    hp.and_then(|p| panel::tray_context_hit(ctx.rect, &ctx.rows, p))
                })
            } else {
                None
            };
        let wifi_hover: Option<WifiAction> =
            if animating_popup == ActivePopup::Wifi && target_progress > 0.0 {
                let wrect = panel::wifi_popup_rect(bar, theme, text, &state);
                let conn = state
                    .toggles
                    .iter()
                    .find(|t| t.action == ToggleAction::Wifi && t.active)
                    .map(|t| t.label.as_str());
                hp.and_then(|p| panel::wifi_popup_hit(wrect, theme, p, conn, &wifi_scan.current()))
            } else {
                None
            };
        let hover_targets = (
            panel_hover,
            launcher_view_hover,
            launcher_clear_hover,
            launcher.hover,
            quick_hover,
            wifi_hover,
            tray_hover,
            tray_context_hover,
        );

        let sig = (
            animating_popup as usize,
            (anim_progress * 1000.0) as u32,
            launcher.scroll as i64,
            launcher.search.len(),
            launcher.view as usize,
            launcher.selected.unwrap_or(usize::MAX),
            state.clock.clone(),
            volume_pct,
            volume_muted,
            (
                th,
                notes.placement() as usize,
                icon_theme as usize,
                hover_targets,
                clock_widget_hover,
            ),
        );
        let menu_sig = (
            task_menu.as_ref().map(|m| m.task + 1).unwrap_or(0),
            task_menu_hover.map(|i| i + 1).unwrap_or(0),
            notify_hover.map(|i| i + 1).unwrap_or(0),
            app_menu.as_ref().map(|m| m.app + 1).unwrap_or(0),
            app_menu_hover.map(|i| i + 1).unwrap_or(0),
            apps_stamp,
            security_stamp,
            security_hover.map(|c| c as usize + 1).unwrap_or(0),
        );
        let changed = sig != last_sig || menu_sig != last_menu_sig;
        let animating =
            osd.visible() || notes.visible() || (anim_progress - target_progress).abs() > 0.002;
        let interactive = frame
            .events
            .iter()
            .any(|e| !matches!(e, Event::PointerMotion { .. }));
        let motion_only = !frame.events.is_empty()
            && frame
                .events
                .iter()
                .all(|e| matches!(e, Event::PointerMotion { .. } | Event::PointerLeave));

        // Szybka ścieżka: wyłącznie ruch kursora i nic się nie zmieniło → nie rysujemy
        // ani nie wysyłamy bufora. Bez tego ruch myszy nad otwartym launcherem zajmował
        // procesor na okrągło (odczuwalne jako „zamrożenie” i 5 FPS).
        if motion_only && !animating && !changed {
            last_sig = sig;
            last_menu_sig = menu_sig;
            return;
        }

        // Diagnostyka/zrzuty: pliki w `$XDG_RUNTIME_DIR` otwierają lub zamykają
        // odpowiedni panel bez wstrzykiwania wejścia.
        if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
            for (name, popup) in [
                ("luna-shell-launcher", ActivePopup::Launcher),
                ("luna-shell-quick", ActivePopup::Quick),
            ] {
                let trigger = std::path::Path::new(&rt).join(name);
                if trigger.exists() {
                    let _ = std::fs::remove_file(&trigger);
                    if target_progress == 0.0 {
                        animating_popup = popup;
                        target_progress = 1.0;
                    } else {
                        target_progress = 0.0;
                    }
                    // Stan jest natychmiastowy — kolejna klatka (timer powłoki) go pokaże.
                    anim_progress = target_progress;
                }
            }
            // Diagnostyka: aktywuj pierwszą ikonę tray’a.
            let trigger = std::path::Path::new(&rt).join("luna-shell-tray-activate");
            if trigger.exists() {
                let _ = std::fs::remove_file(&trigger);
                if let Some(item) = state.tray.first().cloned() {
                    with_tray(&tray_slot, |bus| bus.activate(&item, 0, 0));
                }
            }
            // Diagnostyka: kliknij pierwszą akcję menu kontekstowego tray’a.
            let trigger = std::path::Path::new(&rt).join("luna-shell-tray-menu-click");
            if trigger.exists() {
                let _ = std::fs::remove_file(&trigger);
                if let Some(item) = state.tray.first().cloned() {
                    if let Some(rows) = tray_menu_for(&tray_slot, &item) {
                        if let Some(row) = rows.iter().find(|r| r.enabled && !r.separator) {
                            let id = row.id;
                            with_tray(&tray_slot, |bus| bus.menu_event(&item, id));
                        }
                    }
                }
            }
            // Diagnostyka: otwórz menu kontekstowe pierwszej ikony tray’a.
            let trigger = std::path::Path::new(&rt).join("luna-shell-tray-menu");
            if trigger.exists() {
                let _ = std::fs::remove_file(&trigger);
                if let Some(item) = state.tray.first().cloned() {
                    if let Some(rows) = tray_menu_for(&tray_slot, &item) {
                        let anchor = panel::tray_layout(bar, theme, text, &state)
                            .icons
                            .first()
                            .map(|(r, _)| *r)
                            .unwrap_or_else(|| {
                                Rect::from_xywh(
                                    bar.right() - 80.0,
                                    bar.y(),
                                    panel::TRAY_ICON,
                                    THICKNESS,
                                )
                            });
                        let rect = panel::tray_context_rect(anchor, &rows, theme);
                        tray_context = Some(TrayContextState { item, rows, rect });
                        animating_popup = ActivePopup::TrayContext;
                        target_progress = 1.0;
                        anim_progress = 1.0;
                    }
                }
            }
        }

        // ── Rysowanie sceny ──
        let preference = frame.preference();
        let mut ctx = PaintCtx::new(frame.scene, theme, text);

        // Uwaga: **nie rysujemy przygaszonego tła**. Powierzchnia powłoki ma wysokość
        // chrome (680 px), więc takie przygaszenie kończyło się twardą krawędzią
        // w połowie ekranu („cień na pół ekranu”). Pełnoekranowe przygaszenie wymaga
        // powierzchni na cały ekran — do zrobienia osobno.

        panel::paint_panel(&state, &hover, &mut ctx, bar);

        // Rysowanie wysuniętego popupu/launchera z płynnym ruchem (slide)
        if anim_progress > 0.001 {
            match animating_popup {
                ActivePopup::Launcher => {
                    let slide = (1.0 - anim_progress) * -28.0;
                    let cur_larea = Rect::from_xywh(
                        larea.x(),
                        larea.y() + slide,
                        larea.width(),
                        larea.height(),
                    );
                    panel::paint_launcher(
                        &apps, &launcher, &hover, icon_theme, &mut ctx, cur_larea,
                    );
                    if let Some(menu) = &app_menu {
                        panel::paint_app_menu(&mut ctx, menu.rect, &menu.rows, app_menu_hover);
                    }
                }
                ActivePopup::Wifi => {
                    let slide = (1.0 - anim_progress) * -12.0;
                    let wrect = panel::wifi_popup_rect(bar, &frame.theme, text, &state);
                    let cur_wrect = Rect::from_xywh(
                        wrect.x(),
                        wrect.y() + slide,
                        wrect.width(),
                        wrect.height(),
                    );
                    let wifi_on = state
                        .toggles
                        .iter()
                        .find(|t| t.action == ToggleAction::Wifi)
                        .map(|t| t.active)
                        .unwrap_or(true);
                    let conn_ssid = state
                        .toggles
                        .iter()
                        .find(|t| t.action == ToggleAction::Wifi && t.active)
                        .map(|t| t.label.as_str());
                    panel::paint_wifi_popup(
                        &mut ctx,
                        cur_wrect,
                        wifi_on,
                        conn_ssid,
                        &wifi_scan.current(),
                        &hover,
                    );
                }
                ActivePopup::Calendar => {
                    let slide = (1.0 - anim_progress) * -12.0;
                    let crect = panel::calendar_popup_rect(bar, &frame.theme);
                    let cur_crect = Rect::from_xywh(
                        crect.x(),
                        crect.y() + slide,
                        crect.width(),
                        crect.height(),
                    );
                    panel::paint_calendar_popup_with_events(
                        &mut ctx,
                        cur_crect,
                        &state.clock,
                        &state.date,
                        cal_year,
                        cal_month,
                        today_year,
                        today_month,
                        today_day,
                        cal_selected,
                        &calendar_events,
                        holiday_country.unwrap_or(""),
                        true,
                    );
                    let nrect =
                        panel::notification_center_rect(bar, &frame.theme, notes.history_len());
                    let cur_nrect = Rect::from_xywh(
                        nrect.x(),
                        nrect.y() + slide,
                        nrect.width(),
                        nrect.height(),
                    );
                    panel::paint_notification_center(
                        &mut ctx,
                        cur_nrect,
                        notes.history(),
                        dnd,
                        notify_hover,
                    );
                }
                ActivePopup::Quick => {
                    let slide = (1.0 - anim_progress) * -10.0;
                    let qrect = panel::quick_panel_rect(bar, &frame.theme);
                    let cur = Rect::from_xywh(
                        qrect.x(),
                        qrect.y() + slide,
                        qrect.width(),
                        qrect.height(),
                    );
                    let wifi_on = state
                        .toggles
                        .iter()
                        .find(|t| t.action == ToggleAction::Wifi)
                        .map(|t| t.active)
                        .unwrap_or(true);
                    let ssid = state
                        .toggles
                        .iter()
                        .find(|t| t.action == ToggleAction::Wifi && t.active)
                        .map(|t| t.label.as_str());
                    let now_playing = media.current();
                    if stats && now_playing != last_media {
                        match &now_playing {
                            Some(m) => eprintln!("luna-shell: media: {} — {}", m.title, m.artist),
                            None => eprintln!("luna-shell: media: brak odtwarzacza"),
                        }
                        last_media = now_playing.clone();
                    }
                    let media_info = now_playing.as_ref().map(|m| panel::QuickMedia {
                        title: &m.title,
                        artist: &m.artist,
                        playing: m.playing,
                    });
                    let data = panel::QuickData {
                        user: &state.user,
                        preference,
                        dark: frame.theme.appearance == libluna_core::style::Appearance::Dark,
                        wifi_on,
                        ssid,
                        bluetooth: bluetooth_state,
                        airplane: !wifi_on && !matches!(bluetooth_state, Some((true, _))),
                        dnd,
                        night_light,
                        gamma_available: night_available,
                        power_saver,
                        power_fallback: !luna::system::ppd_available(),
                        volume: volume_pct,
                        muted: volume_muted,
                        brightness: brightness_state,
                        sink: audio_sink.as_deref(),
                        battery: battery_state,
                        media: media_info,
                    };
                    panel::paint_quick_panel(&mut ctx, cur, &data, &hover);
                }
                ActivePopup::TaskMenu => {
                    if let Some(menu) = &task_menu {
                        let slide = (1.0 - anim_progress) * -8.0;
                        let cur = Rect::from_xywh(
                            menu.rect.x(),
                            menu.rect.y() + slide,
                            menu.rect.width(),
                            menu.rect.height(),
                        );
                        panel::paint_task_menu(
                            &mut ctx,
                            cur,
                            &menu.labels,
                            &menu.actions,
                            task_menu_hover,
                        );
                    }
                }
                ActivePopup::Tray => {
                    let slide = (1.0 - anim_progress) * -8.0;
                    let rect = panel::tray_menu_rect(bar, &frame.theme, text, &state);
                    let cur =
                        Rect::from_xywh(rect.x(), rect.y() + slide, rect.width(), rect.height());
                    panel::paint_tray_menu(&mut ctx, cur, &state, tray_hover);
                }
                ActivePopup::TrayContext => {
                    if let Some(menu) = &tray_context {
                        let slide = (1.0 - anim_progress) * -8.0;
                        let cur = Rect::from_xywh(
                            menu.rect.x(),
                            menu.rect.y() + slide,
                            menu.rect.width(),
                            menu.rect.height(),
                        );
                        panel::paint_tray_context_menu(
                            &mut ctx,
                            cur,
                            &menu.rows,
                            tray_context_hover,
                        );
                    }
                }
                ActivePopup::None => {}
            }
        }

        if osd.visible() {
            let w = 340.0;
            osd.paint(
                &mut ctx,
                Rect::from_xywh((size.width - w) / 2.0, THICKNESS + 16.0, w, 72.0),
            );
        }
        if !popup_open {
            if let (Some(rect), Some(activity)) = (clock_widget_rect_now, clock_activity.as_ref()) {
                clock_widget::paint(&mut ctx, rect, activity, clock_now, clock_widget_hover);
            }
        }
        notes.paint(
            &mut ctx,
            Rect::from_xywh(0.0, THICKNESS + 8.0, size.width, size.height - THICKNESS),
        );

        // Modal zgody — rysowany jako ostatni, nad launcherem i powiadomieniami.
        if let Some(prompt) = security_snapshot.prompts.first() {
            let layout = panel::security_dialog_layout(size, &frame.theme);
            let capability = libluna_i18n::tr(prompt.capability.label());
            let message = match prompt.capability {
                luna::security::Capability::Microphone => {
                    libluna_i18n::tr_args("{app} is using the microphone", &[("app", &prompt.name)])
                }
                luna::security::Capability::Camera => {
                    libluna_i18n::tr_args("{app} is using the camera", &[("app", &prompt.name)])
                }
                luna::security::Capability::Network => libluna_i18n::tr_args(
                    "{app} wants to send data outside this computer",
                    &[("app", &prompt.name)],
                ),
            };
            let icon = match prompt.capability {
                luna::security::Capability::Microphone => IconName::Microphone,
                luna::security::Capability::Camera => IconName::Camera,
                luna::security::Capability::Network => IconName::Globe,
            };
            panel::paint_security_dialog(
                &mut ctx,
                layout,
                icon,
                &prompt.name,
                &capability,
                &message,
                prompt.remote.as_deref(),
                security_hover,
            );
        }

        // Schowek aktualizujemy dopiero tutaj: wcześniej `theme`/`ctx` pożyczają
        // `frame`, a `copy_to_clipboard` potrzebuje wyłącznego dostępu.
        if let Some(text) = clipboard_pending.take() {
            frame.copy_to_clipboard(text);
        }

        // Sygnatura była policzona przed rysowaniem (`sig`, `changed`, `animating`,
        // `interactive`) — tutaj tylko podejmujemy decyzję o kolejnej klatce.
        if interactive || animating || changed {
            frame.request_frame();
        }
        // Zegar i stan systemu odświeżają się także bez ruchu myszy.
        // Gdy widoczny jest widget licznika, odświeżamy go płynniej.
        let widget_live = clock_widget_rect_now.is_some_and(|_| {
            clock_activity.as_ref().is_some_and(|activity| {
                activity
                    .timer
                    .as_ref()
                    .is_some_and(|timer| timer.running && timer.remaining_at(clock_now) > 0)
                    || activity
                        .stopwatch
                        .as_ref()
                        .is_some_and(|stopwatch| stopwatch.running)
            })
        });
        let activity_exists = clock_activity
            .as_ref()
            .is_some_and(|activity| activity.has_activity(clock_now));
        let keep_alive = if widget_live {
            std::time::Duration::from_millis(250)
        } else if security_modal {
            // Modal zgody: podświetlenie przycisków i szybka reakcja na decyzję.
            std::time::Duration::from_millis(250)
        } else if activity_exists {
            // Aplikacja może właśnie zamknąć się z aktywnym licznikiem.
            std::time::Duration::from_millis(500)
        } else if startup.elapsed() < std::time::Duration::from_secs(8) {
            std::time::Duration::from_millis(500)
        } else if animating_popup != ActivePopup::None && target_progress > 0.0 {
            // Otwarty launcher/menu: szybciej wykrywamy zmiany i odświeżamy podświetlenia.
            std::time::Duration::from_millis(500)
        } else {
            // Instalacja pakietu ma pojawić się w launcherze bez restartu powłoki —
            // 2 s to kompromis między czasem reakcji a bezczynnym wybudzaniem.
            std::time::Duration::from_secs(2)
        };
        frame.request_frame_after(keep_alive);
        last_sig = sig;
        last_menu_sig = menu_sig;

        if stats {
            stat_frames += 1;
            stat_ms += t_frame.elapsed().as_secs_f64() * 1000.0;
            let secs = stat_report.elapsed().as_secs_f64();
            if secs >= 2.0 {
                eprintln!(
                    "luna-shell: {:.1} klatek/s | kod powłoki {:.2} ms/klatke",
                    stat_frames as f64 / secs,
                    stat_ms / stat_frames.max(1) as f64
                );
                stat_frames = 0;
                stat_ms = 0.0;
                stat_report = Instant::now();
            }
        }
    })
}

/// Uchwyt do usługi powiadomień, wypełniany raz w wątku tła (patrz `main`).
type BusSlot = std::sync::Arc<std::sync::Mutex<Option<NotificationsBus>>>;
type TraySlot = std::sync::Arc<std::sync::Mutex<Option<luna::tray::TrayBus>>>;

/// Menu kontekstowe przycisku okna w taskbarze.
struct TaskMenuState {
    task: usize,
    title: String,
    rect: Rect,
    labels: Vec<String>,
    actions: Vec<panel::TaskMenuAction>,
}

/// Otwarte menu kontekstowe ikony tray’a.
struct TrayContextState {
    item: luna::tray::TrayItem,
    rows: Vec<luna::tray::TrayMenuRow>,
    rect: Rect,
}

/// Otwarte menu kontekstowe aplikacji w launcherze.
struct AppMenuState {
    /// Indeks w `apps` z chwili otwarcia menu.
    app: usize,
    rect: Rect,
    rows: Vec<panel::AppMenuRow>,
}

/// Efekt wybranej akcji menu aplikacji.
enum AppMenuEffect {
    None,
    /// Po uruchomieniu/odinstalowaniu zamknij launcher.
    CloseLauncher,
    /// Skopiuj polecenie do schowka (launcher zostaje otwarty).
    CopyCommand(String),
}

/// Pozycje menu kontekstowego aplikacji (PPM w launcherze).
fn app_menu_rows(app: &App, pinned: bool) -> Vec<panel::AppMenuRow> {
    use panel::{AppMenuAction, AppMenuRow};
    let has_exec = app.exec.is_some();
    let installed = app.desktop_path.is_some();
    let favorite = if pinned {
        libluna_i18n::tr("Remove from favorites")
    } else {
        libluna_i18n::tr("Add to favorites")
    };
    vec![
        AppMenuRow {
            label: libluna_i18n::tr("Open"),
            action: Some(AppMenuAction::Launch),
            enabled: has_exec,
            danger: false,
        },
        AppMenuRow {
            label: favorite,
            action: Some(AppMenuAction::ToggleFavorite),
            enabled: true,
            danger: false,
        },
        AppMenuRow::separator(),
        AppMenuRow {
            label: libluna_i18n::tr("Open as administrator"),
            action: Some(AppMenuAction::OpenAsAdmin),
            enabled: has_exec,
            danger: false,
        },
        AppMenuRow {
            label: libluna_i18n::tr("Copy launch command"),
            action: Some(AppMenuAction::CopyCommand),
            enabled: has_exec,
            danger: false,
        },
        AppMenuRow::separator(),
        AppMenuRow {
            label: libluna_i18n::tr("Uninstall"),
            action: Some(AppMenuAction::Uninstall),
            enabled: installed,
            danger: true,
        },
    ]
}

/// Przesuwa podświetlenie menu o `delta`, pomijając separatory i wyłączone wpisy.
fn app_menu_step(
    rows: &[panel::AppMenuRow],
    current: Option<usize>,
    delta: isize,
) -> Option<usize> {
    let selectable: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.enabled && row.action.is_some())
        .map(|(i, _)| i)
        .collect();
    if selectable.is_empty() {
        return None;
    }
    let Some(pos) = current.and_then(|i| selectable.iter().position(|&r| r == i)) else {
        return if delta < 0 {
            selectable.last().copied()
        } else {
            selectable.first().copied()
        };
    };
    let last = selectable.len() as isize - 1;
    let next = (pos as isize + delta).clamp(0, last) as usize;
    Some(selectable[next])
}

/// Wykonuje akcję z menu kontekstowego aplikacji.
fn perform_app_menu(
    action: panel::AppMenuAction,
    app: &App,
    launcher: &mut Launcher,
) -> AppMenuEffect {
    use panel::AppMenuAction as A;
    match action {
        A::Launch => {
            launch_from_launcher(launcher, app);
            AppMenuEffect::CloseLauncher
        }
        A::ToggleFavorite => {
            launcher.toggle_pin(app);
            luna::launcher_conf::save_from(launcher);
            AppMenuEffect::None
        }
        A::OpenAsAdmin => {
            launch_app_as_admin(app);
            AppMenuEffect::CloseLauncher
        }
        A::CopyCommand => app
            .exec
            .clone()
            .map(AppMenuEffect::CopyCommand)
            .unwrap_or(AppMenuEffect::None),
        A::Uninstall => {
            open_store_package(app);
            AppMenuEffect::CloseLauncher
        }
    }
}

/// Przebudowuje listę aplikacji po instalacji pakietu lub zmianie języka.
///
/// Zwraca `true`, gdy lista została zastąpiona. Przy `reset` czyścimy pozycję
/// i przewinięcie (zmiana języka); przy odświeżeniu po instalacji zachowujemy
/// miejsce, w którym użytkownik był, i tylko ograniczamy zaznaczenie do listy.
fn reload_apps(apps: &mut Vec<App>, launcher: &mut Launcher, reset: bool) -> bool {
    let desktop = luna::apps::scan();
    if desktop.is_empty() {
        return false;
    }
    luna::apps::refresh_icon_names();
    *apps = App::from_desktop(&desktop);
    launcher.hover = None;
    if reset {
        launcher.selected = None;
        launcher.scroll = 0.0;
        launcher.carousel = 0;
    } else {
        let len = launcher.filter_apps(apps).len();
        launcher.selected = if len == 0 {
            None
        } else {
            launcher.selected.map(|i| i.min(len - 1))
        };
    }
    true
}

/// Zamienia tytuł okna na jednoliniowy (protokół sterowania jest linijkowy).
fn one_line(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

/// Wysyła polecenie do Selene (`$XDG_RUNTIME_DIR/luna-control`).
fn control_send(line: &str) {
    use std::io::Write;
    let Ok(rt) = std::env::var("XDG_RUNTIME_DIR") else {
        return;
    };
    let path = std::path::Path::new(&rt).join("luna-control");
    if let Ok(mut s) = std::os::unix::net::UnixStream::connect(path) {
        let _ = writeln!(s, "{line}");
    }
}

/// Woła `f` z usługą powiadomień, jeśli jest już zarejestrowana.
fn with_notifications(slot: &BusSlot, f: impl FnOnce(&NotificationsBus)) {
    if let Ok(g) = slot.lock() {
        if let Some(bus) = g.as_ref() {
            f(bus);
        }
    }
}

/// Woła `f` z usługą tray’a, jeśli jest już zarejestrowana.
fn with_tray(slot: &TraySlot, f: impl FnOnce(&luna::tray::TrayBus)) {
    if let Ok(g) = slot.lock() {
        if let Some(bus) = g.as_ref() {
            f(bus);
        }
    }
}

/// Pobiera menu DBusMenu dla pozycji tray’a (jeśli usługa je udostępnia).
fn tray_menu_for(
    slot: &TraySlot,
    item: &luna::tray::TrayItem,
) -> Option<Vec<luna::tray::TrayMenuRow>> {
    slot.lock().ok()?.as_ref()?.menu(item)
}

/// Zamienia powiadomienie z magistrali D-Bus na toast Luny.
fn notification_from(inc: Incoming) -> Notification {
    // `app_icon`/`desktop-entry` z D-Busa bywa dokładniejszy niż sama nazwa aplikacji
    // (np. „Zed” wysyła `dev.zed.Zed`), więc przekazujemy go do doboru ikony.
    let icon = icon_for(
        inc.icon
            .as_deref()
            .or(inc.desktop_entry.as_deref())
            .unwrap_or(&inc.app_name),
    );
    let timeout = inc.timeout_secs();
    let mut n = Notification::new(inc.app_name, inc.summary, inc.body)
        .with_icon(icon)
        .with_id(inc.id)
        .with_actions(inc.actions)
        .with_desktop_entry(inc.desktop_entry);
    n.timeout = timeout;
    n
}

/// Otwiera aplikację, która wysłała powiadomienie.
///
/// Najpierw szuka istniejącego okna po `app_id` (aktywuje je — bez drugiej
/// instancji), a dopiero potem uruchamia wpis `.desktop`. Gdy wpisu nie ma,
/// próbuje dopasować nazwę wyświetlaną z launcher`a.
fn open_notification_source(
    entry: Option<&str>,
    fallback_name: &str,
    toplevels: Option<&luna::toplevels::Toplevels>,
    apps: &[App],
) {
    let stem = entry
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.strip_suffix(".desktop").unwrap_or(value));
    if let Some(stem) = stem {
        if let Some(tl) = toplevels {
            if let Some(index) = tl
                .tasks()
                .iter()
                .position(|task| app_id_matches_entry(&task.app_id, stem))
            {
                tl.show(index);
                return;
            }
        }
        if let Some(app) = apps
            .iter()
            .find(|app| app_entry_stem(app).is_some_and(|name| name.eq_ignore_ascii_case(stem)))
        {
            launch_app(app);
            return;
        }
    }
    if let Some(app) = apps
        .iter()
        .find(|app| app.name.eq_ignore_ascii_case(fallback_name.trim()))
    {
        launch_app(app);
    }
}

/// Nazwa pliku `.desktop` bez rozszerzenia (np. `eu.luna.Store`).
fn app_entry_stem(app: &App) -> Option<&str> {
    app.desktop_path
        .as_deref()
        .and_then(std::path::Path::file_stem)
        .and_then(std::ffi::OsStr::to_str)
}

/// Czy `app_id` okna odnosi się do wpisu `.desktop` o nazwie `stem`.
fn app_id_matches_entry(app_id: &str, stem: &str) -> bool {
    let app_id = app_id.trim().to_lowercase();
    let stem = stem.trim().to_lowercase();
    if app_id.is_empty() || stem.is_empty() {
        return false;
    }
    app_id == stem || app_id.ends_with(&format!(".{stem}")) || stem.ends_with(&format!(".{app_id}"))
}

/// Dobiera ikonę pakietu do nazwy aplikacji wysyłającej powiadomienie.
fn icon_for(app: &str) -> IconName {
    let a = app.to_lowercase();
    if a.contains("file") || a.contains("plik") {
        IconName::Files
    } else if a.contains("term") {
        IconName::Terminal
    } else if a.contains("music") || a.contains("muzy") || a.contains("audio") {
        IconName::Music
    } else if a.contains("setting") || a.contains("ustaw") {
        IconName::Settings
    } else if a.contains("browser") || a.contains("przegl") {
        IconName::Search
    } else if a.contains("zed") || a.contains("code") || a.contains("editor") {
        IconName::Code
    } else {
        IconName::Info
    }
}

/// Buduje przełączniki paska z realnego stanu systemu.
/// Otwiera Ustawienia na stronie Sieć (gdy panel nie może połączyć bez hasła).
fn open_network_settings() {
    let _ = std::process::Command::new("luna-settings")
        .arg("--page")
        .arg("network")
        .spawn();
}

fn build_toggles(s: &luna::system::SystemStatus) -> Vec<Toggle> {
    let mut v = Vec::new();
    // Sieć i dźwięk na pasku to **same ikony** (bez nazwy sieci i bez „Dźwięk”) —
    // tak jak w nowoczesnych pulpitach. Pełna nazwa/SSID żyje w popupie.
    if let Some((on, ssid)) = &s.wifi {
        v.push(Toggle {
            label: ssid.clone().unwrap_or_else(|| "Wi-Fi".to_owned()),
            value: None,
            icon: IconName::Wifi,
            active: *on,
            action: ToggleAction::Wifi,
        });
    } else {
        v.push(Toggle {
            label: "Wi-Fi".to_owned(),
            value: None,
            icon: IconName::Wifi,
            active: false,
            action: ToggleAction::Wifi,
        });
    }
    if let Some((_pct, muted)) = s.volume {
        v.push(Toggle {
            label: libluna_i18n::tr("Sound").to_owned(),
            value: None,
            icon: IconName::Volume,
            active: !muted,
            action: ToggleAction::Volume,
        });
    } else {
        v.push(Toggle {
            label: libluna_i18n::tr("Sound").to_owned(),
            value: None,
            icon: IconName::Volume,
            active: true,
            action: ToggleAction::Volume,
        });
    }
    if let Some(b) = s.battery {
        v.push(Toggle {
            label: libluna_i18n::tr("Battery").to_owned(),
            value: Some(format!("{}%", b.percent)),
            icon: IconName::Battery,
            active: b.charging,
            action: ToggleAction::None,
        });
    }
    if let Some(pct) = s.brightness {
        v.push(Toggle {
            label: libluna_i18n::tr("Brightness").to_owned(),
            value: Some(format!("{pct}%")),
            icon: IconName::Brightness,
            active: pct > 0,
            action: ToggleAction::Brightness,
        });
    }
    v
}

/// Uruchamia aplikację z launchera i zapisuje ją na liście „ostatnio używane”.
fn launch_from_launcher(launcher: &mut Launcher, app: &App) {
    launch_app(app);
    launcher.remember_used(app);
    luna::launcher_conf::save_from(launcher);
}

/// Uruchamia aplikację, obsługując aplikacje konsolowe w emulatorze terminala oraz odpinając we/wy.
///
/// Każde uruchomienie (i każdy błąd) ląduje w `$XDG_STATE_HOME/luna/apps.log` — bez tego
/// „nie odpala się” nie da się zdiagnozować: `spawn` kończy się sukcesem nawet gdy
/// polecenia nie ma, a komunikat ginie w `/dev/null`.
fn launch_app(app: &App) {
    let Some(exec) = app.exec.clone() else { return };
    // Aplikacje GPU (Zed, Electron, Chrome) nie utworzą powierzchni Wayland w Selene —
    // kompozytor nie ma dmabuf/EGL. Uruchamiamy je przez Xwayland (X11), który działa.
    let force_x11 = needs_x11(app);
    // 1) `gio launch` — standardowa ścieżka dla wpisów `.desktop` (obsługuje `Path=`,
    //    kody pól, `DBusActivatable`). Uruchamiamy je w wątku, bo `gio` potrafi zwrócić
    //    błąd PO starcie (np. „Nie można wczytać informacji o programie”) — wtedy
    //    musimy mieć szansę na fallback, a nie blokować powłoki.
    //    Aplikacje terminalowe pomijamy — `gio` nie otworzy im emulatora.
    //    Aplikacji wymuszonych na X11 **nie** uruchamiamy przez `gio`: wpisy z
    //    `DBusActivatable=true` są aktywowane przez magistralę, która ma własne
    //    środowisko (z `WAYLAND_DISPLAY`), więc nasze `env_remove` byłoby ignorowane
    //    i np. Zed dalej wybierałby Wayland/Vulkan.
    if !app.terminal && !force_x11 {
        if let Some(path) = app.desktop_path.clone() {
            let exec = exec.clone();
            std::thread::spawn(move || {
                let mut cmd = std::process::Command::new("gio");
                cmd.arg("launch")
                    .arg(&path)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null());
                if force_x11 {
                    force_x11_env(&mut cmd);
                }
                apply_display(&mut cmd);
                match apps_log() {
                    Some(log) => {
                        cmd.stderr(std::process::Stdio::from(log));
                    }
                    None => {
                        cmd.stderr(std::process::Stdio::null());
                    }
                }
                let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
                if ok {
                    log_apps(&format!(
                        "start (gio launch{}): {}",
                        if force_x11 { ", X11" } else { "" },
                        path.display()
                    ));
                } else {
                    log_apps(&format!(
                        "gio launch nie powiódł się ({}), próbuję Exec: {exec}",
                        path.display()
                    ));
                    launch_direct(&exec, false, force_x11);
                }
            });
            return;
        }
    }
    // 2) Bez `gio`: bezpośrednie `Exec`.
    launch_direct(&exec, app.terminal, force_x11);
}

/// Uruchamia aplikację z uprawnieniami administratora.
///
/// `pkexec` uruchamia agenta polkit Luny (`luna-polkit`), który pokaże okno
/// z prośbą o hasło administratora. Aplikacje GUI uruchamiamy przez X11
/// (Xwayland) — root zwykle nie ma dostępu do gniazda Waylanda użytkownika.
fn launch_app_as_admin(app: &App) {
    let Some(exec) = app.exec.clone() else {
        return;
    };
    let mut cmd = Command::new("pkexec");
    cmd.arg("env");
    if let Ok(display) = std::env::var("DISPLAY") {
        cmd.arg(format!("DISPLAY={display}"));
    }
    if let Ok(auth) = std::env::var("XAUTHORITY") {
        cmd.arg(format!("XAUTHORITY={auth}"));
    }
    // `sh -c` obsługuje argumenty i cudzysłowy z pola `Exec`.
    cmd.arg("sh").arg("-c").arg(format!("exec {exec}"));
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    match apps_log() {
        Some(log) => {
            cmd.stderr(std::process::Stdio::from(log));
        }
        None => {
            cmd.stderr(std::process::Stdio::null());
        }
    }
    match cmd.spawn() {
        Ok(_) => log_apps(&format!("admin (pkexec): {exec}")),
        Err(err) => log_apps(&format!("pkexec nie uruchomił {exec}: {err}")),
    }
}

/// Otwiera Sklep na pakiecie odpowiadającym aplikacji (ścieżka „Odinstaluj”).
///
/// Przekazujemy nazwę pliku `.desktop` (np. `org.gnome.Nautilus`); Sklep sam
/// dopasowuje ją do identyfikatora pakietu i otwiera jego szczegóły, gdzie jest
/// przycisk usuwania. Gdy `Exec` jest bardziej jednoznaczne, używamy też jego.
fn open_store_package(app: &App) {
    let desktop_id = app
        .desktop_path
        .as_ref()
        .and_then(|path| path.file_stem())
        .map(|stem| stem.to_string_lossy().into_owned());
    let exec_id = app
        .exec
        .as_deref()
        .and_then(|exec| exec.split_whitespace().next())
        .map(|bin| {
            std::path::Path::new(bin)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| bin.to_owned())
        });
    let Some(query) = desktop_id.or(exec_id) else {
        return;
    };
    // Najpierw PATH (instalacja systemowa), potem katalog obok `luna-shell`
    // (uruchomienie z drzewa źródeł lub z paczki portable).
    let store_bin = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("luna-store")))
        .filter(|path| path.is_file())
        .map(|path| path.into_os_string())
        .unwrap_or_else(|| "luna-store".into());
    let mut cmd = Command::new(store_bin);
    cmd.arg("--package").arg(&query);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    match apps_log() {
        Some(log) => {
            cmd.stderr(std::process::Stdio::from(log));
        }
        None => {
            cmd.stderr(std::process::Stdio::null());
        }
    }
    match cmd.spawn() {
        Ok(_) => log_apps(&format!("sklep: --package {query}")),
        Err(err) => log_apps(&format!("nie udało się otworzyć Sklepu dla {query}: {err}")),
    }
}

/// Czy aplikacja wymaga XWaylanda (X11).
///
/// Selene jest kompozytorem software'owym i nie oferuje `zwp_linux_dmabuf_v1`/EGL,
/// więc aplikacje renderujące przez GPU nie mogą utworzyć powierzchni Wayland.
/// Domyślnie obejmuje to Zeda, Electrona i Chrome; własne wpisy dodasz zmienną
/// `LUNA_X11_APPS` (np. `LUNA_X11_APPS=moja-aplikacja,inna`).
fn needs_x11(app: &App) -> bool {
    let mut hay =
        format!("{} {}", app.name, app.exec.as_deref().unwrap_or_default()).to_lowercase();
    if let Some(path) = &app.desktop_path {
        hay.push(' ');
        hay.push_str(&path.to_string_lossy().to_lowercase());
    }
    if let Ok(extra) = std::env::var("LUNA_X11_APPS") {
        for token in extra.split([',', ':', ';']) {
            let token = token.trim().to_lowercase();
            if !token.is_empty() && hay.contains(&token) {
                return true;
            }
        }
    }
    const X11_APPS: &[&str] = &[
        "dev.zed.zed",
        "/zed.app/",
        "google-chrome",
        "chromium",
        "code-insiders",
        "/usr/share/code/",
        "electron",
        "discord",
        "slack",
        "spotify",
    ];
    if X11_APPS.iter().any(|needle| hay.contains(needle)) {
        return true;
    }
    // Aplikacje XFCE/GTK3 z tradycyjnym paskiem menu: GTK3 pod Waylandem **zawsze**
    // rysuje własne CSD (nie pyta kompozytora o tryb dekoracji), więc nie da się mu
    // narzucić naszego paska. Przez Xwayland dostają nasz SSD — chyba że naprawdę
    // rysują CSD (`_MOTIF_WM_HINTS`), wtedy nie dublujemy ramki.
    const X11_UI: &[&str] = &[
        "thunar",
        "xfce4-",
        "xfce4_",
        "mousepad",
        "ristretto",
        "screenshooter",
    ];
    X11_UI.iter().any(|needle| hay.contains(needle))
}

/// Ustawia środowisko wymuszające backend X11 (XWayland).
fn force_x11_env(cmd: &mut std::process::Command) {
    cmd.env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET")
        .env("GDK_BACKEND", "x11")
        .env("QT_QPA_PLATFORM", "xcb")
        .env("SDL_VIDEODRIVER", "x11")
        .env("CLUTTER_BACKEND", "x11")
        .env("MOZ_ENABLE_WAYLAND", "0");
    apply_display(cmd);
}

/// Backend X11 dla **terminala**, ale z zachowanym `WAYLAND_DISPLAY`.
///
/// Terminal ma działać na X11 (spójny pasek tytułu), a uruchamiane w nim
/// aplikacje Waylanda muszą widzieć gniazdo Selene.
fn force_x11_terminal_env(cmd: &mut std::process::Command) {
    cmd.env("GDK_BACKEND", "x11")
        .env("QT_QPA_PLATFORM", "xcb")
        .env("SDL_VIDEODRIVER", "x11")
        .env("CLUTTER_BACKEND", "x11")
        .env("MOZ_ENABLE_WAYLAND", "0");
    // Gniazdo: z bieżącego środowiska albo z markera sesji.
    let socket = std::env::var("WAYLAND_DISPLAY")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            std::env::var_os("XDG_RUNTIME_DIR")
                .and_then(|rt| {
                    std::fs::read_to_string(std::path::Path::new(&rt).join("luna-wayland")).ok()
                })
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        });
    if let Some(socket) = socket {
        cmd.env("WAYLAND_DISPLAY", socket);
    }
    apply_display(cmd);
}

/// `DISPLAY` dla uruchamianych aplikacji.
///
/// Sesja startuje powłokę **od razu** po gnieździe Waylanda, zanim Xwayland zdąży
/// zgłosić gotowość, więc w środowisku powłoki może nie być `DISPLAY`. Numer ekranu
/// czytamy wtedy z markera `$XDG_RUNTIME_DIR/luna-display` (Selene zapisuje go, gdy
/// XWM naprawdę połączy się z X11). Sprawdzamy też gniazdo, żeby nie dać martwego `:N`.
fn display_for_children() -> Option<String> {
    if let Ok(d) = std::env::var("DISPLAY") {
        if !d.is_empty() {
            return Some(d);
        }
    }
    let rt = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let num = std::fs::read_to_string(std::path::Path::new(&rt).join("luna-display")).ok()?;
    let num = num.trim();
    if num.is_empty() || !std::path::Path::new(&format!("/tmp/.X11-unix/X{num}")).exists() {
        return None;
    }
    Some(format!(":{num}"))
}

/// Dopisuje `DISPLAY` (jeśli znamy działający ekran X11).
fn apply_display(cmd: &mut std::process::Command) {
    if let Some(display) = display_for_children() {
        cmd.env("DISPLAY", display);
    }
}

/// Emulatory terminala, których używamy dla wpisów `Terminal=true` (w kolejności).
const TERMINALS: &[&str] = &[
    "luna-terminal",
    "xfce4-terminal",
    "karton-terminal",
    "gnome-terminal",
    "konsole",
    "alacritty",
    "kitty",
    "foot",
    "xterm",
];

/// Buduje polecenie „terminal + program” dla wpisów `Terminal=true`.
///
/// Wcześniej jedynym kandydatem był `x-terminal-emulator` (na PaganOS nie istnieje),
/// więc **żaden** program konsolowy nie startował z launchera.
fn terminal_command(exec: &str) -> String {
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(t) = std::env::var("TERMINAL") {
        if !t.trim().is_empty() {
            candidates.push(t);
        }
    }
    candidates.extend(TERMINALS.iter().map(|t| (*t).to_owned()));
    for term in candidates {
        let bin = term.split_whitespace().next().unwrap_or("");
        let found = if bin.contains('/') {
            std::path::Path::new(bin).is_file()
        } else {
            std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).any(|d| d.join(bin).is_file()))
                .unwrap_or(false)
        };
        if found {
            return format!("{term} -e {exec}");
        }
    }
    exec.to_owned()
}

/// Bezpośredni start `sh -c exec …` (fallback i aplikacje terminalowe).
fn launch_direct(exec: &str, terminal: bool, force_x11: bool) {
    // `%U`, `%F` itp. to kody pól `.desktop`; `gio` je rozwija, ale `sh -c` nie —
    // bez tego program dostawał dosłowny argument `%U`.
    let exec = strip_field_codes(exec);
    let line = if terminal {
        terminal_command(&exec)
    } else {
        format!("exec {exec}")
    };
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(&line);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());
    if force_x11 {
        if terminal {
            // Terminal działa przez X11 (nasz SSD), ale jego **dzieci** mają dostać
            // Wayland — inaczej `luna-files`/inne aplikacje Waylanda kończą się
            // `NoCompositor`, a GTK po cichu spada na X11.
            force_x11_terminal_env(&mut cmd);
        } else {
            force_x11_env(&mut cmd);
        }
    } else {
        apply_display(&mut cmd);
    }
    // Stderr aplikacji kierujemy do logu — tam trafia „command not found”, brak dyspleja itp.
    match apps_log() {
        Some(log) => {
            cmd.stderr(std::process::Stdio::from(log));
        }
        None => {
            cmd.stderr(std::process::Stdio::null());
        }
    }
    match cmd.spawn() {
        Ok(_) => log_apps(&format!(
            "start{}: {line}",
            if force_x11 { " (X11)" } else { "" }
        )),
        Err(e) => log_apps(&format!("BŁĄD startu ({exec}): {e}")),
    }
}

/// Usuwa kody pól `.desktop` (`%U`, `%F`, `%i`, …) z linii `Exec`.
///
/// Zostawiamy `%%` jako literalny znak `%` (tak definiuje to specyfikacja).
fn strip_field_codes(exec: &str) -> String {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('%') => {
                chars.next();
                out.push('%');
            }
            Some(f) if f.is_ascii_alphabetic() => {
                chars.next();
            }
            _ => out.push('%'),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Uchwyt logu uruchamiania aplikacji (dopisywanie).
fn apps_log() -> Option<std::fs::File> {
    let base = std::env::var("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .ok()
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
            std::path::PathBuf::from(home).join(".local/state")
        });
    let dir = base.join("luna");
    let _ = std::fs::create_dir_all(&dir);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("apps.log"))
        .ok()
}

/// Dopisuje znacznikowaną godziną linię do logu uruchamiania aplikacji.
fn log_apps(msg: &str) {
    use std::io::Write;
    if let Some(mut f) = apps_log() {
        let _ = writeln!(f, "{} luna-shell: {}", now_clock_full().0, msg);
    }
}

/// Bieżąca godzina i data w czasie lokalnym — `("HH:MM", "DD.MM", rok, miesiąc, dzień)`.
/// Otwiera zegar na stronie pasującej do aktywnego licznika.
fn open_clock_activity(activity: &ClockActivity, now: i64) {
    let tab = if activity
        .timer
        .as_ref()
        .is_some_and(|timer| timer.is_active(now))
    {
        "timer"
    } else if activity
        .stopwatch
        .as_ref()
        .is_some_and(|stopwatch| stopwatch.is_active(now))
    {
        "stopwatch"
    } else {
        "clock"
    };
    let mut command = Command::new("luna-clock");
    command.arg("--tab").arg(tab);
    if let Err(err) = command.spawn() {
        eprintln!("luna-shell: nie udało się uruchomić luna-clock: {err}");
    }
}

/// Otwiera pełny kalendarz na wybranym dniu (albo na bieżącym miesiącu).
fn open_calendar_app(year: i64, month: u32, day: Option<u32>) {
    let mut command = Command::new("luna-clock");
    if let Some(day) = day {
        command
            .arg("--date")
            .arg(format!("{year:04}-{month:02}-{day:02}"));
    }
    if let Err(err) = command.spawn() {
        eprintln!("luna-shell: nie udało się uruchomić luna-clock: {err}");
    }
}

fn now_clock_full() -> (String, String, i64, u32, u32) {
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    let res = unsafe { libc::localtime_r(&t, tm.as_mut_ptr()) };
    if !res.is_null() {
        let tm = unsafe { tm.assume_init() };
        let h = tm.tm_hour;
        let m = tm.tm_min;
        let y = tm.tm_year as i64 + 1900;
        let mo = tm.tm_mon as u32 + 1;
        let d = tm.tm_mday as u32;
        (
            format!("{h:02}:{m:02}"),
            format!("{d:02}.{mo:02}"),
            y,
            mo,
            d,
        )
    } else {
        let secs = t as u64;
        let days = (secs / 86_400) as i64;
        let secs_of_day = secs % 86_400;
        let (h, m) = (secs_of_day / 3600, (secs_of_day % 3600) / 60);
        let (y, mo, d) = civil_from_days(days);
        (
            format!("{h:02}:{m:02}"),
            format!("{d:02}.{mo:02}"),
            y,
            mo,
            d,
        )
    }
}

/// Data cywilna z liczby dni od 1970-01-01 (algorytm Hinnanta).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Tłumaczy keysym XKB na znak tekstowy do wyszukiwania w launcherze.
fn keysym_to_char(keysym: u32) -> Option<char> {
    match keysym {
        // ASCII (bez znaków sterujących).
        0x20..=0x7e => char::from_u32(keysym),
        // Latin-1 (np. ó, ä, ö, ü) — te keysymy pokrywają się z Unicode.
        0x00a0..=0x00ff => char::from_u32(keysym),
        // Latin Extended-A/B (polskie ą, ć, ę, ł, ń, ś, ź, ż) i dalsze bloki XKB,
        // których numeracja jest zgodna z Unicode.
        0x0100..=0x17ff => char::from_u32(keysym),
        // Jawny zakres Unicode w XKB (0x01000000 + U+XXXX).
        0x01000000..=0x0110ffff => char::from_u32(keysym - 0x01000000),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, exec: &str) -> App {
        App {
            icon: IconName::App,
            theme_icon: None,
            name: name.to_owned(),
            category: String::new(),
            exec: Some(exec.to_owned()),
            terminal: false,
            desktop_path: None,
        }
    }

    #[test]
    fn direct_launch_strips_desktop_field_codes() {
        assert_eq!(strip_field_codes("zed %U"), "zed");
        assert_eq!(
            strip_field_codes("thunar %F --new-window"),
            "thunar --new-window"
        );
        assert_eq!(strip_field_codes("app %%U %f"), "app %U");
        assert_eq!(strip_field_codes("app --file=%f"), "app --file=");
    }

    #[test]
    fn gpu_apps_use_xwayland() {
        assert!(needs_x11(&app(
            "Zed",
            "/home/user/.local/zed.app/bin/zed %U"
        )));
        assert!(needs_x11(&app("Google Chrome", "google-chrome-stable %U")));
    }

    #[test]
    fn xfce_apps_use_xwayland_for_our_decorations() {
        // GTK3 pod Waylandem zawsze rysuje własne CSD; przez X11 dostaje nasz pasek.
        assert!(needs_x11(&app("Thunar", "thunar %U")));
        assert!(needs_x11(&app("Terminal", "xfce4-terminal")));
        assert!(needs_x11(&app("Mousepad", "mousepad %F")));
    }

    #[test]
    fn native_wayland_apps_stay_on_wayland() {
        assert!(!needs_x11(&app("Luna — widgety", "luna-showcase")));
    }

    #[test]
    fn notification_app_ids_match_desktop_entries() {
        assert!(app_id_matches_entry("eu.luna.Store", "eu.luna.Store"));
        assert!(app_id_matches_entry("eu.luna.Store", "Store"));
        assert!(app_id_matches_entry("Store", "eu.luna.Store"));
        assert!(!app_id_matches_entry("org.gnome.Nautilus", "eu.luna.Store"));
        assert!(!app_id_matches_entry("", "eu.luna.Store"));
    }
}
