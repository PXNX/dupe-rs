use crate::app::DupeApp;
use crate::reencode::image_codec::ImageTarget;
use crate::reencode::video_codec::VideoMode;
use crate::reencode::worker::{OriginalHandling, Outcome};
use crate::reencode::{ReencodeJob, ReencodeState};
use crate::ui::format::format_eta;
use egui::{Color32, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons;
use humansize::{DECIMAL, format_size};

const ROW_HEIGHT: f32 = 22.0;

pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    ui.heading("Re-encode");
    ui.label(
        "Shrink images (and optionally videos) by re-encoding them into more efficient \
         formats without losing any quality. A result is only kept when it is actually \
         smaller; images are checked pixel for pixel against the original first.",
    );
    ui.add_space(8.0);

    let state = &mut app.reencode;
    let mut pick = false;
    show_options(state, ui, &mut pick);
    if pick {
        app.start_pick(crate::ui::dialogs::PickPurpose::ReencodeFolders);
    }
    let state = &mut app.reencode;
    ui.add_space(8.0);
    if show_controls(state, ui) {
        app.pending_confirm = Some(crate::app::ConfirmAction::CancelReencode);
    }
    let state = &mut app.reencode;
    if let Some(job) = &mut state.job {
        ui.add_space(4.0);
        show_progress(job, ui);
    }
    ui.add_space(4.0);
    show_totals(state, ui);
    ui.separator();
    show_results(state, ui);
}

/// Sets `pick` when "Add Folder(s)" is clicked.
fn show_options(state: &mut ReencodeState, ui: &mut Ui, pick: &mut bool) {
    let editable = !state.is_running();
    ui.group(|ui| {
        ui.set_min_width(ui.available_width());
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal(|ui| {
                ui.label(icons::ICON_FOLDER_OPEN.rich_text());
                ui.strong("Folders");
                if ui
                    .button(RichText::from(format!(
                        "{} Add Folder(s)",
                        icons::ICON_FOLDER_OPEN.codepoint
                    )))
                    .clicked()
                {
                    *pick = true;
                }
            });
            let mut remove = None;
            for (i, folder) in state.folders.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui
                        .small_button(icons::ICON_CLOSE.codepoint)
                        .on_hover_text("Remove")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                    ui.label(format!(
                        "{} {}",
                        icons::ICON_FOLDER.codepoint,
                        folder.display()
                    ));
                });
            }
            if let Some(i) = remove {
                state.folders.remove(i);
            }
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                ui.label(icons::ICON_IMAGE.rich_text());
                ui.checkbox(&mut state.images_enabled, "Images")
                    .on_hover_text(
                        "PNG, BMP, TIFF, TGA, PNM and QOI files. JPEGs are left alone: they're \
                         already lossy, so any lossless re-encode would be bigger.",
                    );
                for target in [ImageTarget::LosslessWebp, ImageTarget::OptimizedPng] {
                    ui.radio_value(&mut state.image_target, target, target.label());
                }
            });
            ui.horizontal(|ui| {
                ui.label(icons::ICON_MOVIE.rich_text());
                ui.label("Videos:");
                for mode in [
                    VideoMode::Skip,
                    VideoMode::LosslessOnly,
                    VideoMode::VisuallyLossless,
                ] {
                    ui.radio_value(&mut state.video_mode, mode, mode.label());
                }
            });
            if state.video_mode != VideoMode::Skip && !state.ffmpeg_available() {
                ui.colored_label(
                    Color32::from_rgb(230, 160, 60),
                    "ffmpeg and ffprobe aren't on PATH, so videos will be skipped.",
                );
            }
            if state.video_mode == VideoMode::VisuallyLossless {
                ui.colored_label(
                    Color32::from_rgb(230, 160, 60),
                    "Visually lossless is not bit-exact: frames are re-compressed at a quality \
                     meant to be indistinguishable, but the original data is not preserved.",
                );
            }
            ui.horizontal(|ui| {
                ui.label(icons::ICON_DELETE.rich_text());
                ui.label("Originals:");
                ui.radio_value(
                    &mut state.originals,
                    OriginalHandling::Trash,
                    "Move to Recycle Bin",
                );
                ui.radio_value(
                    &mut state.originals,
                    OriginalHandling::Keep,
                    "Keep next to new file",
                );
            });
        });
    });
}

/// Returns whether Cancel was clicked.
fn show_controls(state: &mut ReencodeState, ui: &mut Ui) -> bool {
    let mut cancel = false;
    ui.horizontal(|ui| {
        let can_start = !state.is_running()
            && !state.folders.is_empty()
            && (state.images_enabled || state.video_mode != VideoMode::Skip);
        if !state.is_running()
            && ui
                .add_enabled(
                    can_start,
                    egui::Button::new(RichText::from(format!(
                        "{} Start",
                        icons::ICON_COMPRESS.codepoint
                    ))),
                )
                .clicked()
        {
            state.start();
        }
        if let Some(job) = &mut state.job
            && !job.is_cancelled()
        {
            if ui
                .button(RichText::from(format!(
                    "{} Cancel",
                    icons::ICON_STOP.codepoint
                )))
                .clicked()
            {
                cancel = true;
            }
            if crate::ui::controls::pause_resume_button(ui, job.is_paused())
                .on_hover_text("A video already being encoded finishes first")
                .clicked()
            {
                job.toggle_pause();
            }
        }
    });
    cancel
}

fn show_progress(job: &ReencodeJob, ui: &mut Ui) {
    ui.group(|ui| {
        ui.set_min_width(ui.available_width());
        let Some(total_files) = job.total_files else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Looking for images and videos...");
            });
            return;
        };
        ui.add(egui::ProgressBar::new(job.fraction()).text(format!(
            "{} / {} ({total_files} file(s))",
            format_size(
                (job.fraction() as f64 * job.total_bytes as f64) as u64,
                DECIMAL
            ),
            format_size(job.total_bytes, DECIMAL),
        )));
        let current = job
            .current
            .as_ref()
            .map_or_else(|| "—".to_owned(), |(p, _)| p.display().to_string());
        ui.horizontal(|ui| {
            ui.label(if job.is_paused() {
                "Paused before:"
            } else {
                "Re-encoding:"
            });
            ui.add(egui::Label::new(RichText::new(current).monospace()).truncate());
        });
        let speed = job.bytes_per_sec().map_or_else(
            || "—".to_owned(),
            |r| format!("{}/s", format_size(r as u64, DECIMAL)),
        );
        let eta = job
            .eta()
            .map_or_else(|| "estimating...".to_owned(), format_eta);
        ui.label(format!("Speed: {speed}   ETA: {eta}"));
    });
}

fn show_totals(state: &mut ReencodeState, ui: &mut Ui) {
    let t = state.totals;
    ui.horizontal(|ui| {
        if t.converted > 0 {
            let pct = t.saved() as f64 / t.bytes_before.max(1) as f64 * 100.0;
            ui.colored_label(
                Color32::from_rgb(120, 200, 120),
                format!(
                    "Saved {} ({pct:.1}%) across {} file(s)",
                    format_size(t.saved(), DECIMAL),
                    t.converted
                ),
            );
        }
        if t.skipped + t.failed > 0 {
            ui.label(format!("{} skipped, {} failed", t.skipped, t.failed));
        }
        if !state.results.is_empty() {
            ui.checkbox(&mut state.hide_skipped, "Hide skipped");
        }
        if let Some(msg) = &state.status {
            ui.separator();
            ui.label(msg);
        }
    });
}

fn show_results(state: &ReencodeState, ui: &mut Ui) {
    let rows: Vec<_> = state
        .results
        .iter()
        .filter(|r| !(state.hide_skipped && matches!(r.outcome, Outcome::Skipped(_))))
        .collect();
    if rows.is_empty() {
        return;
    }
    TableBuilder::new(ui)
        .id_salt("reencode_results")
        .striped(true)
        .stick_to_bottom(true)
        .column(Column::remainder().at_least(240.0).resizable(true))
        .column(Column::auto().at_least(90.0))
        .column(Column::auto().at_least(90.0))
        .column(Column::remainder().at_least(160.0))
        .header(20.0, |mut header| {
            for label in ["File", "Before", "After", "Result"] {
                header.col(|ui| {
                    ui.strong(label);
                });
            }
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                let r = rows[row.index()];
                row.col(|ui| {
                    ui.add(egui::Label::new(r.path.display().to_string()).truncate());
                });
                row.col(|ui| {
                    ui.label(format_size(r.size, DECIMAL));
                });
                row.col(|ui| {
                    if let Outcome::Saved { new_size, .. } = &r.outcome {
                        ui.label(format_size(*new_size, DECIMAL));
                    }
                });
                row.col(|ui| match &r.outcome {
                    Outcome::Saved { new_path, new_size } => {
                        let pct = (1.0 - *new_size as f64 / r.size.max(1) as f64) * 100.0;
                        ui.colored_label(Color32::from_rgb(120, 200, 120), format!("-{pct:.1}%"))
                            .on_hover_text(new_path.display().to_string());
                    }
                    Outcome::Skipped(why) => {
                        ui.add(egui::Label::new(RichText::new(why).weak()).truncate());
                    }
                    Outcome::Failed(why) => {
                        ui.add(
                            egui::Label::new(
                                RichText::new(why).color(Color32::from_rgb(255, 120, 120)),
                            )
                            .truncate(),
                        );
                    }
                });
            });
        });
}
