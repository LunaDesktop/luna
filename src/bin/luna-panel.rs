//! Podgląd powłoki Luny (**bez kompozytora**) — „zrzuty ekranu” do weryfikacji
//! wyglądu i układu bez uruchamiania sesji DRM.
//!
//! Renderuje **ten sam** pasek, launcher i popupy, których używa `luna-shell`, przez
//! `libluna-render` — w rozmiarze realnej powierzchni „chrome” (1920×680). Na wierzchu
//! kładzie tło pulpitu, żeby widać było półprzezroczyste przyciemnienie pod popupem.
//!
//! Uruchom: `cargo run -p luna --bin luna-panel`
//! Efekt:   `docs/images/luna-*.png`
//!
//! To jest narzędzie diagnostyczne: każda zmiana wyglądu paska/launchera powinna tu
//! zostawić ślad, który da się obejrzeć bez przejmowania ekranu.

use std::error::Error;

use libluna_core::clock_state::{ClockActivity, StopwatchActivity, TimerActivity};
use libluna_core::{Color, IconTheme, Rect, Size, Theme};
use libluna_render::{gpu::GpuRenderer, Renderer, Scene, SoftwareRenderer, TextEngine};
use libluna_widgets::{Hover, IconName, PaintCtx};
use luna::calendar::CalendarEvent;
use luna::network::{Security, WifiNetwork};
use luna::notifications::{Notification, Notifications};
use luna::osd::Osd;
use luna::panel::{self, App, Launcher, LauncherView, PanelState, LAUNCHER_H, THICKNESS};

/// Szerokość powierzchni (Full HD).
const W: u32 = 1920;
/// Wysokość powierzchni „chrome” — taka sama jak `CHROME_H` w `luna-shell`.
const H: u32 = 680;

fn main() -> Result<(), Box<dyn Error>> {
    let icons = libluna_core::IconSettings::load();
    libluna_core::icons::set_active_icon_set(icons.external);
    libluna_core::icons::set_active_icon_theme(icons.theme);
    let theme = Theme::dark();
    let apps = App::sample();
    let bar = Rect::from_xywh(0.0, 0.0, W as f64, THICKNESS);
    let mut st = PanelState::sample();
    st.tasks.clear();
    let hover = Hover::new();

    let area = |w: f64| {
        let lw = w.min((W as f64 - 80.0).max(320.0));
        Rect::from_xywh((W as f64 - lw) / 2.0, THICKNESS + 24.0, lw, LAUNCHER_H)
    };

    // ── 1) Pasek w spoczynku ────────────────────────────────────────────────────
    save(&theme, "docs/images/luna-panel-idle.png", |ctx| {
        panel::paint_panel(&st, &hover, ctx, bar);
    })?;

    // ── 2) Launcher w trzech widokach ───────────────────────────────────────────
    for (name, view) in [
        ("list", LauncherView::List),
        ("grid", LauncherView::Grid),
        ("carousel", LauncherView::Carousel),
    ] {
        let mut launcher = Launcher::new();
        launcher.view = view;
        launcher.carousel = 2;
        // Podgląd nowego stanu: przypięte na górze (z pinezką) i ostatnio używane.
        launcher.pinned = vec![apps[6].key(), apps[2].key()];
        launcher.recent = vec![apps[3].key()];
        launcher.selected = Some(if view == LauncherView::List { 1 } else { 0 });
        launcher.scroll = 0.0;
        let larea = area(panel::LAUNCHER_W);
        save(
            &theme,
            &format!("docs/images/luna-launcher-{name}.png"),
            |ctx| {
                panel::paint_panel(&st, &hover, ctx, bar);
                panel::paint_launcher(&apps, &launcher, &hover, IconTheme::default(), ctx, larea);
            },
        )?;
    }

    // ── 2b) Launcher z aktywnym wyszukiwaniem (przycisk „×”) ────────────────────
    let mut search_launcher = Launcher::new();
    search_launcher.view = LauncherView::List;
    search_launcher.search = "ter".to_owned();
    search_launcher.selected = Some(0);
    let larea = area(panel::LAUNCHER_W);
    save(&theme, "docs/images/luna-launcher-search.png", |ctx| {
        panel::paint_panel(&st, &hover, ctx, bar);
        panel::paint_launcher(
            &apps,
            &search_launcher,
            &hover,
            IconTheme::default(),
            ctx,
            larea,
        );
    })?;

    // ── 3) Popupy: Wi-Fi, dźwięk, kalendarz, sesja ──────────────────────────────
    let demo = |ssid: &str, signal: u8, active: bool, security: Security| WifiNetwork {
        ssid: ssid.to_owned(),
        signal,
        security,
        active,
        device: "/org/freedesktop/NetworkManager/Devices/3".to_owned(),
        device_name: "wlp12s0f3u2".to_owned(),
        ap_path: "/org/freedesktop/NetworkManager/AccessPoint/1".to_owned(),
        frequency: 5220,
        known: active,
    };
    let networks = vec![
        demo("PaganNet", 92, true, Security::Wpa2),
        demo("Dom", 71, false, Security::Wpa2),
        demo("Sąsiad_5G", 44, false, Security::Wpa3),
    ];
    save(&theme, "docs/images/luna-popup-wifi.png", |ctx| {
        panel::paint_panel(&st, &hover, ctx, bar);
        let r = panel::wifi_popup_rect(bar, ctx.theme, ctx.text, &st);
        panel::paint_wifi_popup(ctx, r, true, Some("PaganNet"), &networks, &hover);
    })?;

    save(&theme, "docs/images/luna-popup-volume.png", |ctx| {
        panel::paint_panel(&st, &hover, ctx, bar);
        let r = panel::volume_popup_rect(bar, ctx.theme, ctx.text, &st);
        panel::paint_volume_popup(ctx, r, 65, false, &hover);
    })?;

    let calendar_events = vec![
        CalendarEvent {
            date: "2026-10-08".to_owned(),
            start_minute: Some(9 * 60 + 30),
            title: "Wizyta u lekarza".to_owned(),
        },
        CalendarEvent {
            date: "2026-10-08".to_owned(),
            start_minute: Some(18 * 60),
            title: "Kolacja z Anią".to_owned(),
        },
        CalendarEvent {
            date: "2026-10-12".to_owned(),
            start_minute: None,
            title: "Urodziny Ali".to_owned(),
        },
    ];

    save(&theme, "docs/images/luna-popup-calendar.png", |ctx| {
        panel::paint_panel(&st, &hover, ctx, bar);
        let r = panel::calendar_popup_rect(bar, ctx.theme);
        panel::paint_calendar_popup_with_events(
            ctx,
            r,
            &st.clock,
            &st.date,
            2026,
            10,
            2026,
            10,
            8,
            Some(8),
            &calendar_events,
            "PL",
            true,
        );
    })?;

    // Kalendarz panelu ze świętami (listopad: Wszystkich Świętych i 11 listopada).
    save(
        &theme,
        "docs/images/luna-popup-calendar-holidays.png",
        |ctx| {
            panel::paint_panel(&st, &hover, ctx, bar);
            let r = panel::calendar_popup_rect(bar, ctx.theme);
            panel::paint_calendar_popup_with_events(
                ctx,
                r,
                &st.clock,
                &st.date,
                2026,
                11,
                2026,
                10,
                8,
                Some(1),
                &[],
                "PL",
                true,
            );
        },
    )?;

    // Widget aktywnego minutnika/stopera pod paskiem (aplikacja zamknięta).
    save(&theme, "docs/images/luna-clock-widget.png", |ctx| {
        panel::paint_panel(&st, &hover, ctx, bar);
        let now = libluna_core::clock_state::now_unix();
        let activity = ClockActivity {
            pid: None,
            updated_unix: now,
            timer: Some(TimerActivity {
                total_secs: 1_500,
                running: true,
                end_unix: now + 17 * 60 + 42,
                remaining_secs: 0,
                finished: false,
            }),
            stopwatch: Some(StopwatchActivity {
                elapsed_millis: 83_400,
                running: true,
                updated_unix: now,
                laps: Vec::new(),
            }),
        };
        let area = Size::new(W as f64, H as f64);
        if let Some(rect) = luna::clock_widget::visible_rect(area, &activity, now) {
            luna::clock_widget::paint(ctx, rect, &activity, now, false);
        }
    })?;

    save(&theme, "docs/images/luna-popup-session.png", |ctx| {
        panel::paint_panel(&st, &hover, ctx, bar);
        let items = luna::power::items();
        panel::paint_session_menu(ctx, bar, &items, &hover);
    })?;

    // Kalendarz + centrum powiadomień (klik w zegar).
    save(&theme, "docs/images/luna-popup-notifications.png", |ctx| {
        let mut nst = PanelState::sample();
        let mut notes = Notifications::new();
        notes.push(
            Notification::new("Pliki", "Kopiowanie zakończone", "12 plików → Dokumenty")
                .with_icon(IconName::Files),
        );
        notes.push(
            Notification::new(
                "System",
                "Aktualizacja gotowa",
                "Uruchom ponownie, aby zainstalować",
            )
            .with_icon(IconName::Info),
        );
        notes.push(
            Notification::new(
                "Terminal",
                "Polecenie zakończone",
                "cargo test — 412 testów",
            )
            .with_icon(IconName::Terminal),
        );
        nst.notifications = notes.history_len();
        panel::paint_panel(&nst, &hover, ctx, bar);
        let crect = panel::calendar_popup_rect(bar, ctx.theme);
        panel::paint_calendar_popup_with_events(
            ctx,
            crect,
            &nst.clock,
            &nst.date,
            2026,
            10,
            2026,
            10,
            8,
            Some(8),
            &calendar_events,
            "PL",
            true,
        );
        let nrect = panel::notification_center_rect(bar, ctx.theme, notes.history_len());
        panel::paint_notification_center(ctx, nrect, notes.history(), false, Some(0));
    })?;

    // ── 4) OSD (głośność) + powiadomienia ───────────────────────────────────────
    save(&theme, "docs/images/luna-osd.png", |ctx| {
        panel::paint_panel(&st, &hover, ctx, bar);
        let mut osd = Osd::new(IconName::Volume, "Głośność", 0.65);
        osd.advance(0.3);
        osd.paint(
            ctx,
            Rect::from_xywh((W as f64 - 340.0) / 2.0, THICKNESS + 16.0, 340.0, 72.0),
        );
        let mut notes = Notifications::new();
        notes.push(
            Notification::new("Pliki", "Kopiowanie zakończone", "12 plików → Dokumenty")
                .with_icon(IconName::Files),
        );
        notes.advance(0.3);
        notes.paint(ctx, Rect::from_xywh(0.0, THICKNESS + 8.0, W as f64, 200.0));
    })?;

    Ok(())
}

/// Rysuje tło pulpitu + zawartość + zapisuje PNG.
fn save(theme: &Theme, path: &str, draw: impl FnOnce(&mut PaintCtx)) -> Result<(), Box<dyn Error>> {
    let text = TextEngine::system();
    let mut scene = Scene::new();
    {
        let mut ctx = PaintCtx::new(&mut scene, theme, &text);
        // Tło pulpitu (pod półprzezroczystą powierzchnią chrome).
        ctx.scene
            .fill_rect(Rect::from_size(Size::new(W as f64, H as f64)), desktop_bg());
        draw(&mut ctx);
    }
    let mut renderer: Box<dyn Renderer> = match std::env::var("LUNA_GPU") {
        Ok(v) if !v.is_empty() && v != "0" => match GpuRenderer::new(W, H) {
            Ok(r) => {
                eprintln!("luna-panel: backend GPU");
                Box::new(r)
            }
            Err(e) => {
                eprintln!("luna-panel: GPU niedostępne ({e}) — backend CPU");
                Box::new(SoftwareRenderer::new(W, H))
            }
        },
        _ => Box::new(SoftwareRenderer::new(W, H)),
    };
    renderer.draw(&scene, 1.0, theme.colors.window_bg);
    let png = renderer.encode_png().ok_or("failed to encode PNG")?;
    std::fs::write(path, png)?;
    println!("saved {path} ({W}x{H})");
    Ok(())
}

/// Kolor „pulpitu” pod powierzchnią chrome (żeby przyciemnienie było widoczne).
fn desktop_bg() -> Color {
    Color::from_rgba8(0x1a, 0x1d, 0x27, 0xff)
}
