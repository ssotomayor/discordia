use chrono::{Datelike, Duration, NaiveDate};
use dioxus::prelude::*;

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];
const MINUTE_STEP: u32 = 5;

const TRIGGER: &str = "w-full flex items-center gap-2 bg-[var(--panel2)] border border-[var(--edge-strong)] rounded-[10px] px-3 py-2 text-[12.5px] text-[var(--text)] text-left hover:border-[var(--accent)] transition-colors";
const NAV: &str = "w-7 h-7 flex items-center justify-center rounded-md text-[var(--text-muted)] hover:bg-white/[0.06] hover:text-[var(--text)] transition-colors";

fn month_name(month: u32) -> String {
    crate::i18n::label(MONTHS[(month as usize).saturating_sub(1) % 12]).to_string()
}

/// Day, month, year in the order the language writes them.
pub(crate) fn shown_date(date: NaiveDate) -> String {
    crate::i18n::text(
        "{0} {1} {2}",
        &[
            date.day().to_string(),
            month_name(date.month()),
            date.year().to_string(),
        ],
    )
}

fn shown_month(first: NaiveDate) -> String {
    crate::i18n::text(
        "{0} {1}",
        &[month_name(first.month()), first.year().to_string()],
    )
}

fn first_of(date: NaiveDate) -> NaiveDate {
    date.with_day(1).unwrap_or(date)
}

fn shift_month(first: NaiveDate, by: i32) -> NaiveDate {
    let months = first.year() * 12 + first.month0() as i32 + by;
    NaiveDate::from_ymd_opt(months.div_euclid(12), months.rem_euclid(12) as u32 + 1, 1)
        .unwrap_or(first)
}

/// Six weeks from the Monday on or before the 1st, so every month fits the same grid.
pub(crate) fn month_grid(first: NaiveDate) -> Vec<NaiveDate> {
    let start = first - Duration::days(i64::from(first.weekday().num_days_from_monday()));
    (0..42).map(|i| start + Duration::days(i)).collect()
}

/// Floats under its trigger in the browser's top layer. Inline, it pushed the
/// dialog open; positioned in the dialog, its scrolling body would clip it.
#[component]
fn Floating(id: String, dismiss: String, label: String, children: Element) -> Element {
    let script = format!(
        "({})({id:?});",
        include_str!("../../assets/anchored-popover.js")
    );
    rsx! {
        div {
            id: "{id}",
            "popover": "manual",
            "data-dismiss": "{dismiss}",
            role: "dialog",
            aria_label: "{label}",
            // No display class here: it would beat the hidden state of a closed popover.
            class: "rounded-xl border border-[var(--border)] bg-[var(--panel-solid)] p-2.5 shadow-lg text-[var(--text)] select-none",
            style: "position:fixed;inset:auto;margin:0;max-width:calc(100vw - 16px);",
            onmounted: move |_| {
                let _ = document::eval(&script);
            },
            {children}
        }
    }
}

fn popover_id(kind: &str) -> String {
    format!("dxf-{kind}-{}", uuid::Uuid::new_v4().simple())
}

#[component]
pub fn DatePicker(
    value: NaiveDate,
    on_change: EventHandler<NaiveDate>,
    #[props(default)] min: Option<NaiveDate>,
    label: String,
) -> Element {
    let mut open = use_signal(|| false);
    let mut month = use_signal(|| first_of(value));
    let id = use_hook(|| popover_id("calendar"));
    let dismiss = super::dismiss::use_dismiss(open);
    let today = chrono::Local::now().date_naive();
    let shown = month();
    rsx! {
        div { class: "relative",
            button {
                r#type: "button",
                class: TRIGGER,
                aria_label: "{label}",
                aria_expanded: open(),
                aria_haspopup: "dialog",
                "data-dismiss": "{dismiss}",
                onclick: move |_| {
                    month.set(first_of(value));
                    open.toggle();
                },
                span { class: "block w-3.5 h-3.5 shrink-0 text-[var(--text-muted)]", dangerous_inner_html: super::icons::CALENDAR }
                span { class: "truncate", {shown_date(value)} }
            }
            if open() {
                Floating { id: id.clone(), dismiss: dismiss.clone(), label: label.clone(),
                    div { class: "w-[16.5rem]",
                        div { class: "flex items-center justify-between mb-1.5",
                            button { r#type: "button", class: NAV, title: crate::i18n::text("Previous month", &[]),
                                onclick: move |_| month.set(shift_month(shown, -1)), "‹"
                            }
                            span { class: "text-xs font-medium", {shown_month(shown)} }
                            button { r#type: "button", class: NAV, title: crate::i18n::text("Next month", &[]),
                                onclick: move |_| month.set(shift_month(shown, 1)), "›"
                            }
                        }
                        div { class: "grid grid-cols-7 gap-0.5 mb-0.5",
                            for day in WEEKDAYS {
                                span { class: "h-6 flex items-center justify-center text-[10px] uppercase tracking-wider text-[var(--text-dim)]",
                                    {crate::i18n::label(day)}
                                }
                            }
                        }
                        div { class: "grid grid-cols-7 gap-0.5", role: "grid",
                            for date in month_grid(shown) {
                                {
                                    let selected = date == value;
                                    let outside = date.month() != shown.month();
                                    let disabled = min.is_some_and(|m| date < m);
                                    let class = if selected {
                                        "h-8 rounded-md text-xs font-semibold bg-[var(--accent)] text-[var(--bg)]"
                                    } else if date == today {
                                        "h-8 rounded-md text-xs text-[var(--text)] ring-1 ring-inset ring-[var(--accent)] hover:bg-white/[0.06] disabled:opacity-30"
                                    } else if outside {
                                        "h-8 rounded-md text-xs text-[var(--text-dim)] opacity-50 hover:bg-white/[0.06] disabled:opacity-20"
                                    } else {
                                        "h-8 rounded-md text-xs text-[var(--text)] hover:bg-white/[0.06] disabled:opacity-30"
                                    };
                                    rsx! {
                                        button {
                                            key: "{date}",
                                            r#type: "button",
                                            class,
                                            disabled,
                                            aria_selected: "{selected}",
                                            onclick: move |_| {
                                                on_change.call(date);
                                                open.set(false);
                                            },
                                            "{date.day()}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

const TIME_CELL: &str =
    "w-full h-8 shrink-0 rounded-md text-xs tabular-nums text-[var(--text)] hover:bg-white/[0.06]";
const TIME_CELL_ON: &str = "w-full h-8 shrink-0 rounded-md text-xs font-semibold tabular-nums bg-[var(--accent)] text-[var(--bg)]";

/// Two scrolling columns, hours and minutes; the minute closes it, as the time is then whole.
#[component]
pub fn TimePicker(
    hour: u32,
    minute: u32,
    on_change: EventHandler<(u32, u32)>,
    label: String,
) -> Element {
    let mut open = use_signal(|| false);
    let id = use_hook(|| popover_id("time"));
    let dismiss = super::dismiss::use_dismiss(open);
    rsx! {
        div { class: "relative",
            button {
                r#type: "button",
                class: TRIGGER,
                aria_label: "{label}",
                aria_expanded: open(),
                aria_haspopup: "dialog",
                "data-dismiss": "{dismiss}",
                onclick: move |_| open.toggle(),
                span { class: "block w-3.5 h-3.5 shrink-0 text-[var(--text-muted)]", dangerous_inner_html: super::icons::CLOCK }
                span { class: "tabular-nums", "{hour:02}:{minute:02}" }
            }
            if open() {
                Floating { id: id.clone(), dismiss: dismiss.clone(), label: label.clone(),
                    div { class: "flex gap-1.5",
                        div { class: "w-14 flex flex-col",
                            span { class: "h-6 flex items-center justify-center text-[10px] uppercase tracking-wider text-[var(--text-dim)]", {crate::i18n::text("Hour", &[])} }
                            div { class: "dxf-time-column h-52 overflow-y-auto flex flex-col gap-0.5 pr-0.5", role: "listbox", aria_label: crate::i18n::text("Hour", &[]),
                                for h in 0..24u32 {
                                    button {
                                        key: "h{h}",
                                        r#type: "button",
                                        role: "option",
                                        class: if h == hour { TIME_CELL_ON } else { TIME_CELL },
                                        aria_selected: "{h == hour}",
                                        onclick: move |_| on_change.call((h, minute)),
                                        "{h:02}"
                                    }
                                }
                            }
                        }
                        div { class: "w-px bg-[var(--border)] my-1" }
                        div { class: "w-14 flex flex-col",
                            span { class: "h-6 flex items-center justify-center text-[10px] uppercase tracking-wider text-[var(--text-dim)]", {crate::i18n::text("Minute", &[])} }
                            div { class: "dxf-time-column h-52 overflow-y-auto flex flex-col gap-0.5 pr-0.5", role: "listbox", aria_label: crate::i18n::text("Minute", &[]),
                                for m in (0..60u32).step_by(MINUTE_STEP as usize) {
                                    button {
                                        key: "m{m}",
                                        r#type: "button",
                                        role: "option",
                                        class: if m == minute { TIME_CELL_ON } else { TIME_CELL },
                                        aria_selected: "{m == minute}",
                                        onclick: move |_| {
                                            on_change.call((hour, m));
                                            open.set(false);
                                        },
                                        "{m:02}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn a_month_grid_starts_on_monday_and_holds_six_weeks() {
        let grid = month_grid(date(2026, 11, 1));
        assert_eq!(grid.len(), 42);
        assert_eq!(grid[0], date(2026, 10, 26), "1 Nov 2026 is a Sunday");
        assert!(grid.contains(&date(2026, 11, 30)));
        let grid = month_grid(date(2026, 6, 1));
        assert_eq!(
            grid[0],
            date(2026, 6, 1),
            "a month starting on Monday starts the grid"
        );
    }

    #[test]
    fn months_step_across_years() {
        assert_eq!(shift_month(date(2026, 12, 1), 1), date(2027, 1, 1));
        assert_eq!(shift_month(date(2026, 1, 1), -1), date(2025, 12, 1));
    }
}
