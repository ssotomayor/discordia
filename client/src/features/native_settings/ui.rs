use super::{Edit, Event, Session, Snapshot};
use eframe::egui::{self, Color32, RichText, Stroke, Ui};
use std::sync::{Arc, atomic::Ordering};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Audio,
    Mic,
    Video,
    Activity,
    Diagnostics,
}

const TABS: &[(Tab, &str, &str)] = &[
    (Tab::Audio, "Voice", "Audio"),
    (Tab::Mic, "", "Microphone"),
    (Tab::Video, "Picture", "Video and screen"),
    (Tab::Activity, "Status", "Level and activity"),
    (Tab::Diagnostics, "", "Diagnostics"),
];

pub(super) fn run(session: Arc<Session>) -> Result<(), String> {
    use winit::platform::windows::EventLoopBuilderExtWindows;
    let icon = image::load_from_memory(include_bytes!("../../../assets/icon-1024.png"))
        .map_err(|e| format!("Couldn't load Settings icon: {e}"))?
        .into_rgba8();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Discordia — Settings")
            .with_inner_size([780.0, 560.0])
            .with_min_inner_size([640.0, 440.0])
            .with_icon(egui::IconData {
                width: icon.width(),
                height: icon.height(),
                rgba: icon.into_raw(),
            }),
        event_loop_builder: Some(Box::new(|builder| {
            builder.with_any_thread(true);
        })),
        renderer: eframe::Renderer::Glow,
        centered: true,
        run_and_return: true,
        ..Default::default()
    };
    eframe::run_native(
        "Discordia — Settings",
        options,
        Box::new(move |cc| {
            if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
                let mut fonts = egui::FontDefinitions::default();
                fonts
                    .font_data
                    .insert("Segoe UI".into(), egui::FontData::from_owned(bytes).into());
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "Segoe UI".into());
                cc.egui_ctx.set_fonts(fonts);
            }
            *session.context.lock() = Some(cc.egui_ctx.clone());
            let view = session
                .snapshot
                .lock()
                .take()
                .ok_or_else(|| std::io::Error::other("Missing Settings state"))?;
            Ok(Box::new(SettingsApp {
                session,
                view,
                tab: Tab::Audio,
                sequence: 0,
                edits: Vec::new(),
                stats: false,
                executable: String::new(),
                game_name: String::new(),
                error: None,
                #[cfg(test)]
                probe: std::env::var_os("DISCORDIA_NATIVE_SETTINGS_PROBE").map(|dir| Probe {
                    directory: dir.into(),
                    index: 0,
                    frames: 0,
                    waiting: false,
                }),
            }))
        }),
    )
    .map_err(|e| format!("Couldn't open native Settings: {e}"))
}

struct SettingsApp {
    session: Arc<Session>,
    view: Snapshot,
    tab: Tab,
    sequence: u64,
    edits: Vec<(Edit, bool)>,
    stats: bool,
    executable: String,
    game_name: String,
    error: Option<String>,
    #[cfg(test)]
    probe: Option<Probe>,
}

#[cfg(test)]
struct Probe {
    directory: std::path::PathBuf,
    index: usize,
    frames: u32,
    waiting: bool,
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(mut next) = self.session.snapshot.lock().take() {
            // Delayed meter snapshots must not undo an edit awaiting the main thread.
            if next.ack < self.sequence {
                next.prefs = self.view.prefs.clone();
            }
            self.view = next;
        }
        if self.session.close.load(Ordering::Acquire)
            || ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let mut appearance = self.view.prefs.clone();
        appearance.accent = self.view.accent.clone();
        theme(ctx, &appearance);
        egui::SidePanel::left("settings-tabs")
            .exact_width(190.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(color(&self.view.prefs, "--panel2"))
                    .inner_margin(12),
            )
            .show(ctx, |ui| {
                ui.add_space(8.0);
                for &(tab, group, label) in TABS {
                    if !group.is_empty() {
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new(group.to_uppercase())
                                .size(10.0)
                                .color(color(&self.view.prefs, "--text-dim")),
                        );
                        ui.add_space(4.0);
                    }
                    let selected = self.tab == tab;
                    let response = ui.add_sized(
                        [ui.available_width(), 36.0],
                        egui::Button::new(label).selected(selected),
                    );
                    if response.clicked() {
                        self.tab = tab;
                    }
                    tab_icon(
                        ui,
                        tab,
                        response.rect.left_center() + egui::vec2(15.0, 0.0),
                        color(
                            &appearance,
                            if selected { "--text" } else { "--text-muted" },
                        ),
                    );
                    if selected {
                        let rect = response.rect;
                        ui.painter().line_segment(
                            [
                                rect.left_top() + egui::vec2(0.0, 5.0),
                                rect.left_bottom() - egui::vec2(0.0, 5.0),
                            ],
                            Stroke::new(3.0_f32, color(&appearance, "--accent")),
                        );
                    }
                }
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(color(&self.view.prefs, "--panel-solid"))
                    .inner_margin(20),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().slider_width = (ui.available_width() - 72.0).max(100.0);
                egui::ScrollArea::vertical()
                    .id_salt(match self.tab {
                        Tab::Audio => 0,
                        Tab::Mic => 1,
                        Tab::Video => 2,
                        Tab::Activity => 3,
                        Tab::Diagnostics => 4,
                    })
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.tab {
                        Tab::Audio => self.audio(ui),
                        Tab::Mic => self.microphone(ui),
                        Tab::Video => self.video(ui),
                        Tab::Activity => self.activity(ui),
                        Tab::Diagnostics => self.diagnostics(ui),
                    });
            });
        let polling = self.tab == Tab::Diagnostics && self.stats;
        if self.stats && self.tab != Tab::Diagnostics {
            self.stats = false;
            self.session.send(Event::Stats(false));
        }
        if polling {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        for (edit, persist) in self.edits.drain(..) {
            if let Err(error) = edit.apply(&mut self.view.prefs) {
                self.error = Some(error.into());
                continue;
            }
            self.sequence += 1;
            self.session.send(Event::Edit(self.sequence, edit, persist));
        }
        #[cfg(test)]
        if let Some(mut probe) = self.probe.take() {
            let screenshots = ctx.input(|input| {
                input
                    .raw
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::Screenshot { image, .. } => Some(image.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            });
            for image in screenshots {
                let names = ["audio", "microphone", "video", "activity", "diagnostics"];
                let bytes: Vec<_> = image
                    .pixels
                    .iter()
                    .flat_map(|pixel| pixel.to_array())
                    .collect();
                if let Err(error) = std::fs::create_dir_all(&probe.directory)
                    .map_err(image::ImageError::IoError)
                    .and_then(|()| {
                        image::save_buffer(
                            probe.directory.join(format!("{}.png", names[probe.index])),
                            &bytes,
                            image.size[0] as u32,
                            image.size[1] as u32,
                            image::ColorType::Rgba8,
                        )
                    })
                {
                    self.session.send(Event::Failed(error.to_string()));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                probe.index += 1;
                probe.frames = 0;
                probe.waiting = false;
                if probe.index == TABS.len() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                self.tab = TABS[probe.index].0;
                if self.tab == Tab::Diagnostics {
                    self.stats = true;
                }
            }
            probe.frames += 1;
            if probe.frames >= 4 && !probe.waiting {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                probe.waiting = true;
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            self.probe = Some(probe);
        }
    }
}

fn heading(ui: &mut Ui, title: &str, help: &str) {
    ui.heading(RichText::new(title).size(21.0).strong());
    ui.add_space(3.0);
    ui.label(
        RichText::new(help)
            .size(12.0)
            .color(ui.visuals().weak_text_color()),
    );
    ui.add_space(18.0);
}

fn tab_icon(ui: &Ui, tab: Tab, center: egui::Pos2, tint: Color32) {
    let p = ui.painter();
    let stroke = Stroke::new(1.3_f32, tint);
    let point = |x, y| center + egui::vec2(x, y);
    let line = |a: (f32, f32), b: (f32, f32)| {
        p.line_segment([point(a.0, a.1), point(b.0, b.1)], stroke);
    };
    match tab {
        Tab::Audio => {
            p.add(egui::Shape::closed_line(
                vec![
                    point(-6.0, -2.5),
                    point(-2.0, -2.5),
                    point(2.0, -6.0),
                    point(2.0, 6.0),
                    point(-2.0, 2.5),
                    point(-6.0, 2.5),
                ],
                stroke,
            ));
            line((5.0, -3.5), (6.5, 0.0));
            line((6.5, 0.0), (5.0, 3.5));
        }
        Tab::Mic => {
            p.rect_stroke(
                egui::Rect::from_center_size(point(0.0, -2.0), egui::vec2(5.0, 9.0)),
                3,
                stroke,
                egui::StrokeKind::Inside,
            );
            p.add(egui::Shape::line(
                vec![
                    point(-5.0, -1.0),
                    point(-5.0, 3.0),
                    point(0.0, 5.0),
                    point(5.0, 3.0),
                    point(5.0, -1.0),
                ],
                stroke,
            ));
            line((0.0, 5.0), (0.0, 8.0));
            line((-3.0, 8.0), (3.0, 8.0));
        }
        Tab::Video => {
            p.rect_stroke(
                egui::Rect::from_center_size(point(-1.0, 0.0), egui::vec2(10.0, 9.0)),
                2,
                stroke,
                egui::StrokeKind::Inside,
            );
            p.add(egui::Shape::closed_line(
                vec![
                    point(4.0, -2.0),
                    point(8.0, -4.0),
                    point(8.0, 4.0),
                    point(4.0, 2.0),
                ],
                stroke,
            ));
        }
        Tab::Activity => {
            p.rect_stroke(
                egui::Rect::from_center_size(center, egui::vec2(16.0, 10.0)),
                4,
                stroke,
                egui::StrokeKind::Inside,
            );
            line((-6.0, 0.0), (-2.0, 0.0));
            line((-4.0, -2.0), (-4.0, 2.0));
            p.circle_filled(point(4.0, 1.5), 1.0, tint);
            p.circle_filled(point(6.0, -1.5), 1.0, tint);
        }
        Tab::Diagnostics => {
            p.add(egui::Shape::line(
                vec![
                    point(-8.0, 0.0),
                    point(-4.0, 0.0),
                    point(-2.0, -6.0),
                    point(1.0, 6.0),
                    point(4.0, -2.0),
                    point(6.0, 0.0),
                    point(8.0, 0.0),
                ],
                stroke,
            ));
        }
    }
}

fn hint(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(11.0)
            .color(ui.visuals().weak_text_color()),
    );
    ui.add_space(7.0);
}

fn device(
    ui: &mut Ui,
    id: &str,
    label: &str,
    value: &mut Option<String>,
    items: &[(String, String)],
    enabled: bool,
) -> bool {
    ui.label(label);
    let before = value.clone();
    ui.add_enabled_ui(enabled, |ui| {
        let caption = items
            .iter()
            .find(|(key, _)| Some(key) == value.as_ref())
            .map(|(_, label)| label.as_str())
            .unwrap_or_else(|| value.as_deref().unwrap_or("System default"));
        egui::ComboBox::from_id_salt(id)
            .width(ui.available_width())
            .selected_text(caption)
            .show_ui(ui, |ui| {
                ui.selectable_value(value, None, "System default");
                for (key, label) in items {
                    ui.selectable_value(value, Some(key.clone()), label);
                }
            });
    });
    ui.add_space(8.0);
    *value != before
}

fn slider(
    ui: &mut Ui,
    label: &str,
    value: &mut u32,
    range: std::ops::RangeInclusive<u32>,
    suffix: &str,
) -> Option<bool> {
    ui.label(label);
    let response = ui
        .scope(|ui| {
            let accent = ui.visuals().selection.stroke.color;
            let visuals = ui.visuals_mut();
            visuals.selection.bg_fill = accent;
            visuals.widgets.inactive.bg_fill = accent;
            visuals.widgets.hovered.bg_fill = accent;
            visuals.widgets.active.bg_fill = accent;
            ui.add(
                egui::Slider::new(value, range)
                    .suffix(suffix)
                    .handle_shape(egui::style::HandleShape::Circle)
                    .trailing_fill(true),
            )
        })
        .inner;
    ui.add_space(6.0);
    (response.changed() || response.drag_stopped()).then(|| !response.dragged())
}

fn toggle(ui: &mut Ui, label: &str, value: &mut bool, hint_text: Option<&str>) -> bool {
    let changed = ui.checkbox(value, label).changed();
    if let Some(text) = hint_text {
        hint(ui, text);
    } else {
        ui.add_space(6.0);
    }
    changed
}

impl SettingsApp {
    fn audio(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Audio",
            "Choose your output device and adjust app sounds and soundboard volume independently.",
        );
        let items: Vec<_> = self
            .view
            .outputs
            .iter()
            .map(|s| (s.clone(), s.clone()))
            .collect();
        if device(
            ui,
            "output",
            "Output",
            &mut self.view.prefs.selected_output_device,
            &items,
            !self.view.reconnecting,
        ) {
            self.edits.push((
                Edit::Output(self.view.prefs.selected_output_device.clone()),
                true,
            ));
        }
        ui.add_space(8.0);
        egui::Frame::group(ui.style())
            .inner_margin(12)
            .show(ui, |ui| {
                ui.strong("App sounds");
                hint(
                    ui,
                    "Messages, incoming and outgoing calls, and interface notifications.",
                );
                let mut volume = self.view.prefs.sfx_volume as u32;
                if let Some(save) = slider(ui, "Volume", &mut volume, 0..=100, "%") {
                    self.view.prefs.sfx_volume = volume as u8;
                    self.edits.push((Edit::AppVolume(volume as u8), save));
                }
                if ui.button("Test app sound").clicked() {
                    self.session.send(Event::TestSound);
                }
            });
        ui.add_space(12.0);
        egui::Frame::group(ui.style()).inner_margin(12).show(ui, |ui| {
            ui.strong("Soundboard volume");
            let mut volume = self.view.prefs.soundboard_volume as u32;
            if let Some(save) = slider(ui,"Playback",&mut volume,0..=100,"%") { self.view.prefs.soundboard_volume = volume as u8; self.edits.push((Edit::Soundboard(volume as u8),save)); }
            hint(ui,"Soundboard playback only. App notifications and call sounds use the separate control above. Muting someone also mutes their soundboard.");
        });
    }

    fn microphone(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Microphone",
            "What others hear, when the gate opens, and what is processed before it leaves.",
        );
        let items: Vec<_> = self
            .view
            .inputs
            .iter()
            .map(|s| (s.clone(), s.clone()))
            .collect();
        if device(
            ui,
            "input",
            "Input",
            &mut self.view.prefs.selected_input_device,
            &items,
            !self.view.reconnecting,
        ) {
            self.edits.push((
                Edit::Input(self.view.prefs.selected_input_device.clone()),
                true,
            ));
        }
        let level_caption = if self.view.microphone_error {
            "Microphone unavailable — retrying".into()
        } else if self.view.reconnecting {
            "Reconnecting".into()
        } else if self.view.microphone_active {
            super::super::voice::peak_to_db_label(self.view.level)
        } else {
            "Join voice to see the input level".into()
        };
        ui.horizontal(|ui| {
            ui.label("Level");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(level_caption).size(11.0));
            });
        });
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 8.0), egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, 4, color(&self.view.prefs, "--bg2"));
        if self.view.microphone_active {
            let pct = super::super::voice::peak_to_meter_pct(self.view.level) as f32 / 100.0;
            let fill =
                egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * pct, rect.height()));
            ui.painter()
                .rect_filled(fill, 4, color(&self.view.prefs, "--up"));
            let threshold = super::super::voice::peak_to_meter_pct(self.view.prefs.mic_sensitivity)
                as f32
                / 100.0;
            let x = rect.left() + rect.width() * threshold;
            ui.painter().line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                Stroke::new(2.0_f32, Color32::WHITE),
            );
            if self.view.prefs.noise_cancellation && self.view.pre_level != self.view.level {
                let x = rect.left()
                    + rect.width()
                        * super::super::voice::peak_to_meter_pct(self.view.pre_level) as f32
                        / 100.0;
                ui.painter().line_segment(
                    [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                    Stroke::new(2.0_f32, color(&self.view.prefs, "--text-dim")),
                );
            }
        }
        hint(
            ui,
            "Grey tick: before noise suppression. Sensitivity is measured before automatic gain and microphone volume.",
        );
        let mut volume = self.view.prefs.mic_volume as u32;
        if let Some(save) = slider(ui, "Microphone Volume", &mut volume, 0..=200, "%") {
            self.view.prefs.mic_volume = volume as u16;
            self.edits.push((Edit::MicVolume(volume as u16), save));
        }
        hint(
            ui,
            if self.view.prefs.auto_gain_control {
                "Volume adjusts the level after automatic gain. Peaks are limited to prevent clipping."
            } else {
                "Volume boosts or reduces your microphone level. Peaks are limited to prevent clipping."
            },
        );
        let mut sensitivity =
            super::super::voice::peak_to_meter_pct(self.view.prefs.mic_sensitivity);
        if let Some(save) = slider(ui, "Sensitivity", &mut sensitivity, 0..=100, "%") {
            let peak = super::super::voice::meter_pct_to_peak(sensitivity);
            self.view.prefs.mic_sensitivity = peak;
            self.edits.push((Edit::Sensitivity(peak), save));
        }
        ui.label(
            RichText::new(super::super::voice::peak_to_db_label(
                self.view.prefs.mic_sensitivity,
            ))
            .size(11.0),
        );
        if self.view.microphone_active {
            hint(
                ui,
                if self.view.muted {
                    "Muted"
                } else if self.view.speaking {
                    "Transmitting"
                } else {
                    "Below threshold — not transmitting"
                },
            );
        }
        if crate::rawmic::supported() {
            if toggle(
                ui,
                "Bypass system audio processing",
                &mut self.view.prefs.bypass_system_audio_processing,
                Some(
                    "Skips the suppression and gain your audio driver applies before we hear anything. Reopens the microphone.",
                ),
            ) {
                self.edits.push((
                    Edit::Bypass(self.view.prefs.bypass_system_audio_processing),
                    true,
                ));
            }
            if let Some(error) = &self.view.bypass_error {
                ui.colored_label(
                    color(&self.view.prefs, "--danger"),
                    format!("Couldn't bypass it: {error}. The microphone is open the usual way."),
                );
            }
        }
        if toggle(
            ui,
            "Automatic gain control",
            &mut self.view.prefs.auto_gain_control,
            None,
        ) {
            self.edits
                .push((Edit::Agc(self.view.prefs.auto_gain_control), true));
        }
        if toggle(
            ui,
            "Noise cancellation",
            &mut self.view.prefs.noise_cancellation,
            Some("Removes fans, keyboards and room noise (DeepFilterNet)."),
        ) {
            self.edits
                .push((Edit::Noise(self.view.prefs.noise_cancellation), true));
        }
        if self.view.prefs.noise_cancellation {
            if let Some(save) = slider(
                ui,
                "Suppression strength",
                &mut self.view.prefs.denoise_atten_lim_db,
                super::super::voice::DENOISE_ATTEN_LIM_DB_MIN
                    ..=super::super::voice::DENOISE_ATTEN_LIM_DB_MAX,
                " dB max",
            ) {
                self.edits.push((
                    Edit::Suppression(self.view.prefs.denoise_atten_lim_db),
                    save,
                ));
            }
            hint(
                ui,
                "Lower keeps more of your voice, and more of the room with it.",
            );
        }
        ui.separator();
        ui.add_space(10.0);
        ui.strong("Transmission");
        if self.view.reconnecting {
            ui.colored_label(color(&self.view.prefs, "--warn"), "Reconnecting audio…");
        }
        let before = self.view.prefs.voice_bitrate_kbps;
        egui::ComboBox::from_id_salt("voice-quality")
            .selected_text(if before == 24 {
                "Standard — 24 kbit/s"
            } else {
                "High — 48 kbit/s"
            })
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut self.view.prefs.voice_bitrate_kbps,
                    24,
                    "Standard — 24 kbit/s",
                );
                ui.selectable_value(
                    &mut self.view.prefs.voice_bitrate_kbps,
                    48,
                    "High — 48 kbit/s",
                );
            });
        if before != self.view.prefs.voice_bitrate_kbps {
            self.edits
                .push((Edit::Bitrate(self.view.prefs.voice_bitrate_kbps), true));
        }
        hint(
            ui,
            if self.view.voice_connected {
                "Applies the next time you join a voice channel."
            } else {
                "Higher sounds better on low voices and background music, and costs more upload."
            },
        );
    }

    fn video(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Video and screen",
            "Your camera, and the shape of what you share.",
        );
        if device(
            ui,
            "camera",
            "Camera",
            &mut self.view.prefs.camera_device_id,
            &self.view.cameras,
            true,
        ) {
            self.edits
                .push((Edit::Camera(self.view.prefs.camera_device_id.clone()), true));
        }
        ui.label("Screen share");
        let presets = super::super::screenshare::QUALITY_PRESETS;
        let before = self.view.prefs.screenshare_quality.clone();
        let caption = presets
            .iter()
            .find(|(id, _, _)| *id == before)
            .map(|(_, label, _)| *label)
            .unwrap_or("1080p");
        egui::ComboBox::from_id_salt("screen-quality")
            .width(ui.available_width())
            .selected_text(caption)
            .show_ui(ui, |ui| {
                for &(id, label, _) in presets {
                    ui.selectable_value(&mut self.view.prefs.screenshare_quality, id.into(), label);
                }
            });
        if before != self.view.prefs.screenshare_quality {
            self.edits.push((
                Edit::ScreenQuality(self.view.prefs.screenshare_quality.clone()),
                true,
            ));
        }
        if let Some((_, _, text)) = presets
            .iter()
            .find(|(id, _, _)| *id == self.view.prefs.screenshare_quality)
        {
            hint(ui, text);
        }
        if toggle(
            ui,
            "Share computer sound",
            &mut self.view.prefs.screenshare_audio,
            None,
        ) {
            self.edits
                .push((Edit::ScreenAudio(self.view.prefs.screenshare_audio), true));
        }
        if toggle(
            ui,
            "Adaptive quality for viewers",
            &mut self.view.prefs.screenshare_adaptive_quality,
            Some(
                "Send an additional quality for slower connections. Uses more upload bandwidth and encoding resources. Applies to the next screen share.",
            ),
        ) {
            self.edits.push((
                Edit::AdaptiveQuality(self.view.prefs.screenshare_adaptive_quality),
                true,
            ));
        }
    }

    fn activity(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Level",
            "Each server counts what you earned on it. Only this app can add those up across servers.",
        );
        if toggle(
            ui,
            "Publish my overall level to Nostr",
            &mut self.view.prefs.publish_global_level,
            Some(
                "A number and a count of servers — never which ones. It is self-reported and never gates access.",
            ),
        ) {
            self.edits.push((
                Edit::GlobalLevel(self.view.prefs.publish_global_level),
                true,
            ));
        }
        ui.add_space(12.0);
        heading(
            ui,
            "Game activity",
            "Shows the people you share a guild with what you are playing. Turn off sharing to stop publishing your game activity.",
        );
        if toggle(
            ui,
            "Share what I am playing",
            &mut self.view.prefs.share_activity,
            None,
        ) {
            self.edits
                .push((Edit::ShareActivity(self.view.prefs.share_activity), true));
        }
        ui.add_enabled_ui(self.view.prefs.share_activity,|ui| {
            if toggle(ui,"Detect running games",&mut self.view.prefs.detect_games,Some("Checks every 15 seconds using local Steam, Epic and Ubisoft installations and a built-in list. Only the detected game's name is shared.")) {self.edits.push((Edit::DetectGames(self.view.prefs.detect_games),true));}
            if self.view.prefs.detect_games { hint(ui,&self.view.activity.as_ref().map(|name| format!("Playing {name}")).unwrap_or_else(|| "No game detected yet. Open a game and wait up to 15 seconds.".into())); }
            if toggle(ui,"Accept Discord Rich Presence",&mut self.view.prefs.discord_rpc_socket,Some("Whichever app starts first takes the socket. Restart the app after changing this.")) {self.edits.push((Edit::DiscordPresence(self.view.prefs.discord_rpc_socket),true));}
        });
        ui.add_space(12.0);
        ui.strong("Additional games");
        hint(
            ui,
            "For games the automatic detector misses, add the executable name shown in Task Manager → Details and the name to display.",
        );
        ui.add(
            egui::TextEdit::singleline(&mut self.executable)
                .hint_text("Executable, e.g. game.exe")
                .char_limit(240)
                .desired_width(ui.available_width()),
        );
        ui.add(
            egui::TextEdit::singleline(&mut self.game_name)
                .hint_text("Game name")
                .char_limit(80)
                .desired_width(ui.available_width()),
        );
        if ui
            .add_enabled(
                !self.executable.trim().is_empty() && !self.game_name.trim().is_empty(),
                egui::Button::new("Add game"),
            )
            .clicked()
        {
            let edit = Edit::AddGame(self.executable.clone(), self.game_name.clone());
            let mut candidate = self.view.prefs.clone();
            match edit.apply(&mut candidate) {
                Ok(()) => {
                    self.edits.push((edit, true));
                    self.executable.clear();
                    self.game_name.clear();
                    self.error = None;
                }
                Err(error) => self.error = Some(error.into()),
            }
        }
        if let Some(error) = &self.error {
            ui.colored_label(color(&self.view.prefs, "--danger"), error);
        }
        for (exe, name) in self.view.prefs.detect_extra.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{name} · {exe}"));
                if ui.button("Remove").clicked() {
                    self.edits.push((Edit::RemoveGame(exe), true));
                }
            });
        }
    }

    fn diagnostics(&mut self, ui: &mut Ui) {
        heading(
            ui,
            "Diagnostics",
            "The only thing here that configures nothing: it measures.",
        );
        if ui.checkbox(&mut self.stats, "Connection stats").changed() {
            self.session.send(Event::Stats(self.stats));
        }
        if !self.stats {
            return;
        }
        ui.separator();
        if self.view.voice_stats.is_empty() {
            hint(ui, "Waiting for the first reading — join a voice channel.");
        }
        for (name, stats) in &self.view.voice_stats {
            ui.strong(name);
            let text = match stats {
                crate::state::TrackStats::Inbound {
                    loss_pct,
                    jitter_ms,
                    buffer_ms,
                    concealment_events,
                } => format!(
                    "{loss_pct:.1}% loss · {jitter_ms:.0} ms jitter · {buffer_ms:.0} ms buffer · {concealment_events} concealments"
                ),
                crate::state::TrackStats::Outbound {
                    bitrate_kbps,
                    packets_per_sec,
                    target_kbps,
                } => format!(
                    "{} kbit/s out · {} packets/s · target {target_kbps} kbit/s",
                    number(*bitrate_kbps),
                    number(*packets_per_sec)
                ),
            };
            ui.label(RichText::new(text).monospace().size(11.0));
            ui.add_space(7.0);
        }
        screen_stats(ui, "Screen share outbound", self.view.outbound.as_ref());
        screen_stats(ui, "Screen share inbound", self.view.inbound.as_ref());
    }
}

fn number<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map(|v| v.to_string()).unwrap_or_else(|| "—".into())
}

fn screen_stats(ui: &mut Ui, title: &str, stats: Option<&crate::state::ScreenShareStats>) {
    ui.separator();
    ui.strong(title);
    let Some(stats) = stats else {
        hint(ui, "Waiting for a screen share.");
        return;
    };
    if let Some(error) = &stats.error {
        ui.colored_label(Color32::from_rgb(232, 119, 106), error);
    }
    if stats.outbound {
        ui.label(format!(
            "Capture {}×{} @ {} FPS · processing {} ms/frame",
            number(stats.capture_width),
            number(stats.capture_height),
            stats
                .capture_fps
                .map(|v| format!("{v:.0}"))
                .unwrap_or_else(|| "—".into()),
            stats
                .capture_processing_ms
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "—".into())
        ));
    }
    ui.label(format!(
        "{}×{} @ {} FPS · {} · {} kbit/s",
        number(stats.encoded_width),
        number(stats.encoded_height),
        stats
            .encoded_fps
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "—".into()),
        stats.codec.as_deref().unwrap_or("codec not reported"),
        number(stats.bitrate_kbps)
    ));
    ui.label(format!(
        "{} · power-efficient {}",
        stats
            .codec_implementation
            .as_deref()
            .unwrap_or("implementation not reported"),
        stats
            .power_efficient
            .map(|v| if v { "yes" } else { "no" })
            .unwrap_or("not reported")
    ));
    ui.label(format!(
        "{} frames · {} packets · {} lost",
        number(stats.frames),
        number(stats.packets),
        number(stats.packets_lost)
    ));
    if stats.outbound {
        ui.label(format!(
            "Target {} kbit/s · encode {} ms/frame · limit {}",
            number(stats.target_bitrate_kbps),
            stats
                .encode_ms
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "—".into()),
            stats
                .quality_limitation_reason
                .as_deref()
                .unwrap_or("not reported")
        ));
    } else {
        ui.label(format!(
            "{} ms jitter",
            stats
                .jitter_ms
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "—".into())
        ));
    }
}

fn css_color(value: &str) -> Option<Color32> {
    if let Some(hex) = value.trim().strip_prefix('#')
        && hex.len() == 6
    {
        let n = u32::from_str_radix(hex, 16).ok()?;
        return Some(Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8));
    }
    let rgba = value.trim().strip_prefix("rgba(")?.strip_suffix(')')?;
    let parts: Vec<_> = rgba.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return None;
    }
    Some(Color32::from_rgba_unmultiplied(
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
        (parts[3].parse::<f32>().ok()?.clamp(0.0, 1.0) * 255.0).round() as u8,
    ))
}

fn color(prefs: &crate::settings::ClientSettings, name: &str) -> Color32 {
    if name == "--accent"
        && let Some(value) = prefs.accent.as_deref().and_then(css_color)
    {
        return value;
    }
    crate::app::theme_vars(&prefs.theme)
        .split(';')
        .find_map(|entry| {
            let (key, value) = entry.trim().split_once(':')?;
            (key == name).then(|| css_color(value)).flatten()
        })
        .unwrap_or(Color32::GRAY)
}

fn theme(ctx: &egui::Context, prefs: &crate::settings::ClientSettings) {
    let mut style = egui::Style {
        visuals: if prefs.theme == "daylight" {
            egui::Visuals::light()
        } else {
            egui::Visuals::dark()
        },
        ..Default::default()
    };
    style.visuals.override_text_color = Some(color(prefs, "--text"));
    style.visuals.panel_fill = color(prefs, "--panel-solid");
    style.visuals.window_fill = color(prefs, "--panel2");
    style.visuals.extreme_bg_color = color(prefs, "--bg2");
    style.visuals.faint_bg_color = color(prefs, "--panel2");
    style.visuals.selection.bg_fill = color(prefs, "--accent").linear_multiply(0.22);
    style.visuals.selection.stroke = Stroke::new(1.0_f32, color(prefs, "--accent"));
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, color(prefs, "--border"));
    style.visuals.widgets.noninteractive.fg_stroke =
        Stroke::new(1.0_f32, color(prefs, "--text-muted"));
    style.visuals.widgets.inactive.weak_bg_fill = color(prefs, "--panel2");
    style.visuals.widgets.inactive.bg_fill = color(prefs, "--bg2");
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, color(prefs, "--border"));
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, color(prefs, "--accent"));
    style.visuals.widgets.active.bg_fill = color(prefs, "--accent");
    style.spacing.item_spacing = egui::vec2(8.0, 7.0);
    style.spacing.interact_size = egui::vec2(36.0, 30.0);
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
    ctx.set_style(style);
    ctx.set_zoom_factor(prefs.text_size_percent.clamp(80, 140) as f32 / 100.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_theme_and_custom_accent_are_available_without_css_rendering() {
        for theme in crate::app::THEMES {
            let prefs = crate::settings::ClientSettings {
                theme: theme.id.into(),
                ..Default::default()
            };
            for name in [
                "--panel-solid",
                "--panel2",
                "--border",
                "--text",
                "--text-muted",
                "--accent",
            ] {
                assert_ne!(color(&prefs, name), Color32::GRAY, "{} {name}", theme.id);
            }
        }
        assert_eq!(
            css_color("rgba(110,168,255,.16)"),
            Some(Color32::from_rgba_unmultiplied(110, 168, 255, 41))
        );
        assert!(css_color("invalid").is_none());
    }
}
