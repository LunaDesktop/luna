//! Kompaktowy widget aktywnego minutnika/stopera pod paskiem panelu.
//!
//! Pokazuje się tylko wtedy, gdy `luna-clock` jest zamknięty, a licznik dalej
//! działa. Kliknięcie otwiera aplikację na właściwej stronie. To pierwszy krok
//! w stronę systemu widgetów: stan jest trwały, a powłoka tylko go czyta.

use libluna_core::clock_state::{ClockActivity, TimerActivity};
use libluna_core::style::weight;
use libluna_core::{Color, Point, Rect, Size};
use libluna_widgets::PaintCtx;

use crate::panel::THICKNESS;

const WIDTH: f64 = 312.0;
const MARGIN: f64 = 24.0;
const PAD: f64 = 14.0;
const ROW_H: f64 = 54.0;

/// Prostokąt widgetu (albo `None`, gdy nie powinien być widoczny).
pub fn visible_rect(size: Size, activity: &ClockActivity, now: i64) -> Option<Rect> {
    if !activity.should_show_widget(now) {
        return None;
    }
    let rows = row_count(activity, now);
    if rows == 0 {
        return None;
    }
    let height = PAD * 2.0 + rows as f64 * ROW_H;
    Some(Rect::from_xywh(
        (size.width - WIDTH - MARGIN).max(MARGIN),
        THICKNESS + 12.0,
        WIDTH,
        height,
    ))
}

fn row_count(activity: &ClockActivity, now: i64) -> usize {
    let timer = activity
        .timer
        .as_ref()
        .is_some_and(|timer| timer.is_active(now));
    let stopwatch = activity
        .stopwatch
        .as_ref()
        .is_some_and(|stopwatch| stopwatch.is_active(now));
    usize::from(timer) + usize::from(stopwatch)
}

/// Rysuje widget i zwraca jego prostokąt.
pub fn paint(ctx: &mut PaintCtx, rect: Rect, activity: &ClockActivity, now: i64, hovered: bool) {
    let theme = *ctx.theme;
    let colors = theme.colors;

    // Delikatny cień, żeby karta odcinała się od tła pulpitu.
    ctx.scene.fill_rounded_rect(
        Rect::from_xywh(rect.x() + 2.0, rect.y() + 5.0, rect.width(), rect.height()),
        theme.radius.large,
        Color::rgba(0.0, 0.0, 0.0, 0.20),
    );
    ctx.scene
        .fill_rounded_rect(rect, theme.radius.large, colors.card_bg);
    let border = if hovered {
        colors.accent
    } else {
        colors.border
    };
    ctx.scene
        .stroke_rounded_rect(rect, theme.radius.large, 1.0, border);

    let mut y = rect.y() + PAD;
    if let Some(timer) = &activity.timer {
        if timer.is_active(now) {
            paint_timer_row(ctx, rect, y, timer, now);
            y += ROW_H;
        }
    }
    if let Some(stopwatch) = &activity.stopwatch {
        if stopwatch.is_active(now) {
            let secs = stopwatch.elapsed_at(now) / 1_000;
            paint_row(
                ctx,
                rect,
                y,
                &libluna_i18n::tr("Stopwatch"),
                &format_duration(secs),
                colors.accent,
                None,
            );
        }
    }
}

fn paint_timer_row(ctx: &mut PaintCtx, card: Rect, y: f64, timer: &TimerActivity, now: i64) {
    let theme = *ctx.theme;
    let finished = timer.remaining_at(now) == 0;
    let color = if finished {
        theme.colors.warning
    } else {
        theme.colors.accent
    };
    let label = if finished {
        libluna_i18n::tr("Timer finished")
    } else {
        libluna_i18n::tr("Timer")
    };
    let remaining = timer.remaining_at(now);
    paint_row(
        ctx,
        card,
        y,
        &label,
        &format_duration(remaining),
        color,
        Some(timer.progress(now)),
    );
}

fn paint_row(
    ctx: &mut PaintCtx,
    card: Rect,
    y: f64,
    label: &str,
    time: &str,
    color: Color,
    progress: Option<f64>,
) {
    let theme = *ctx.theme;
    let x = card.x() + PAD;
    let row_w = card.width() - PAD * 2.0;

    ctx.scene
        .fill_circle(Point::new(x + 5.0, y + 17.0), 4.0, color);
    ctx.label_left(
        x + 18.0,
        y + 17.0,
        label,
        theme.typography.caption.size,
        weight::MEDIUM,
        theme.colors.dim_text,
    );
    ctx.label_right(
        card.right() - PAD,
        y + 17.0,
        time,
        theme.typography.heading.size,
        weight::BOLD,
        theme.colors.text,
    );

    if let Some(progress) = progress {
        let bar = Rect::from_xywh(x, y + 38.0, row_w, 5.0);
        ctx.scene.fill_rounded_rect(bar, 2.5, theme.colors.shade);
        let filled = bar.width() * progress.clamp(0.0, 1.0);
        if filled > 0.0 {
            ctx.scene.fill_rounded_rect(
                Rect::from_xywh(bar.x(), bar.y(), filled, bar.height()),
                2.5,
                color,
            );
        }
    }
}

fn format_duration(secs: u64) -> String {
    let hours = secs / 3_600;
    let minutes = (secs % 3_600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libluna_core::clock_state::{StopwatchActivity, TimerActivity};
    use libluna_render::{Scene, TextEngine};

    fn activity(timer: bool, watch: bool) -> ClockActivity {
        ClockActivity {
            pid: None,
            updated_unix: 1_000,
            timer: timer.then_some(TimerActivity {
                total_secs: 300,
                running: true,
                end_unix: 1_300,
                remaining_secs: 0,
                finished: false,
            }),
            stopwatch: watch.then_some(StopwatchActivity {
                elapsed_millis: 5_000,
                running: true,
                updated_unix: 1_000,
                laps: Vec::new(),
            }),
        }
    }

    #[test]
    fn widget_only_when_closed_and_active() {
        let size = Size::new(1920.0, 680.0);
        assert!(visible_rect(size, &activity(true, false), 1_100).is_some());
        assert!(visible_rect(size, &activity(false, false), 1_100).is_none());
    }

    #[test]
    fn finished_timer_moves_out_of_the_widget() {
        let size = Size::new(1920.0, 680.0);
        let mut activity = activity(true, false);
        activity.timer.as_mut().unwrap().end_unix = 900; // now=1100 → po czasie
        assert!(visible_rect(size, &activity, 1_100).is_none());
    }

    #[test]
    fn widget_grows_with_two_rows() {
        let size = Size::new(1920.0, 680.0);
        let one = visible_rect(size, &activity(true, false), 1_100).unwrap();
        let two = visible_rect(size, &activity(true, true), 1_100).unwrap();
        assert!(two.height() > one.height());
        assert_eq!(two.right(), size.width - MARGIN);
    }

    #[test]
    fn duration_format() {
        assert_eq!(format_duration(59), "00:59");
        assert_eq!(format_duration(61), "01:01");
        assert_eq!(format_duration(3_661), "01:01:01");
    }

    #[test]
    fn paints_timer_and_stopwatch_rows() {
        let theme = libluna_core::Theme::dark();
        let text = TextEngine::none();
        let activity = activity(true, true);
        let rect = visible_rect(Size::new(1920.0, 680.0), &activity, 1_100).unwrap();
        let mut scene = Scene::new();
        let mut ctx = PaintCtx::new(&mut scene, &theme, &text);
        paint(&mut ctx, rect, &activity, 1_100, true);
        assert!(scene.len() > 8, "widget powinien narysować oba wiersze");
    }
}
