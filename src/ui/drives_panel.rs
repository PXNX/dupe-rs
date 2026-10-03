use crate::app::DupeApp;
use crate::drives::{DriveRecord, MountedDrive, attention};
use crate::index_db::ContentDiff;
use crate::smart::{self, HealthStatus, SmartAttribute};
use crate::ui::format::{format_ago, format_timestamp};
use crate::volume::{VolumeInfo, format_serial};
use egui::{Color32, Frame, Id, Margin, Modal, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons;
use humansize::{DECIMAL, format_size};
use std::collections::HashMap;
use std::time::SystemTime;

const GOOD: Color32 = Color32::from_rgb(110, 200, 130);
const CAUTION: Color32 = Color32::from_rgb(235, 180, 80);
const DANGER: Color32 = Color32::from_rgb(235, 110, 110);
const MUTED: Color32 = Color32::from_gray(140);

/// What a card asked for, applied once rendering is done borrowing state.
enum Action {
    CheckHealth(Vec<VolumeInfo>),
    Track(MountedDrive),
    SetTwin(String, Option<String>),
    AskForget(String),
    Open(String),
}

pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    let mut actions = Vec::new();

    ui.heading("Drives");
    ui.label(
        "Every drive dupe-rs has indexed, filled or been told to track, including ones that \
         aren't plugged in. Pair drives that hold the same data as twins to see whether \
         they've drifted apart, and check their SMART health now and then.",
    );
    ui.add_space(6.0);
    show_toolbar(app, ui, &mut actions);
    ui.add_space(6.0);

    let now = SystemTime::now();
    let indexed: HashMap<String, (usize, u64)> = app
        .reverse_search
        .db
        .volumes()
        .iter()
        .map(|v| (v.key.clone(), (v.files, v.bytes)))
        .collect();
    let records: Vec<(String, DriveRecord)> = app
        .drives
        .registry
        .sorted()
        .into_iter()
        .map(|(k, r)| (k.clone(), r.clone()))
        .collect();
    let diffs: HashMap<String, ContentDiff> = records
        .iter()
        .filter_map(|(key, r)| {
            let twin = r.twin.as_ref()?;
            let both_indexed = indexed.contains_key(key) && indexed.contains_key(twin);
            both_indexed.then(|| {
                (
                    key.clone(),
                    app.drives.twin_diff(key, twin, &app.reverse_search.db),
                )
            })
        })
        .collect();
    let labels: HashMap<&str, &str> = records
        .iter()
        .map(|(k, r)| (k.as_str(), r.label.as_str()))
        .collect();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if records.is_empty() {
                ui.label(
                    "No drives yet. Index a drive in the Reverse Search tab, fill one in Drive \
                     Fill, or track an attached drive below.",
                );
            }
            for (key, record) in &records {
                let card = Card {
                    key,
                    record,
                    attached: app.drives.mounted_letter(key),
                    indexed: indexed.get(key).copied(),
                    diff: diffs.get(key).copied(),
                    labels: &labels,
                    checking: app.drives.is_checking(key),
                    health_error: app.drives.health_errors.get(key).map(String::as_str),
                    any_check_running: app.drives.is_checking_health(),
                    now,
                };
                card.show(ui, &mut actions);
                ui.add_space(6.0);
            }
            show_untracked(app, ui, &mut actions);
        });

    show_forget_modal(app, ui);

    for action in actions {
        match action {
            Action::CheckHealth(volumes) => app.drives.check_health(volumes),
            Action::Track(drive) => app.drives.track(&drive),
            Action::SetTwin(a, b) => app.drives.set_twin(&a, b.as_deref()),
            Action::AskForget(key) => app.drives.pending_forget = Some(key),
            Action::Open(letter) => {
                if let Err(err) = open::that(format!("{letter}\\")) {
                    app.drives.status = Some(format!("Couldn't open {letter}: {err}"));
                }
            }
        }
    }
}

fn show_toolbar(app: &mut DupeApp, ui: &mut Ui, actions: &mut Vec<Action>) {
    let attached = app.drives.attached_tracked();
    ui.horizontal(|ui| {
        let checking = app.drives.is_checking_health();
        if ui
            .add_enabled(
                !checking && !attached.is_empty(),
                egui::Button::new(format!(
                    "{} Check health of attached drives",
                    icons::ICON_MONITOR_HEART.codepoint
                )),
            )
            .on_hover_text(
                "Reads the SMART data of every attached tracked drive. Windows asks for \
                 administrator rights once per check.",
            )
            .clicked()
        {
            actions.push(Action::CheckHealth(attached.clone()));
        }
        if ui
            .button(format!("{} Refresh", icons::ICON_REFRESH.codepoint))
            .on_hover_text("Re-read the attached drives' names (e.g. after renaming one)")
            .clicked()
        {
            app.drives.refresh();
        }
        if checking {
            ui.spinner();
            ui.label("Reading drive health; approve the Windows prompt if it asks...");
        }
    });

    let total = app.drives.registry.len();
    if total > 0 {
        ui.weak(format!(
            "{total} drive(s) tracked, {} attached right now.",
            attached.len()
        ));
    }
    if let Some(status) = &app.drives.status {
        ui.label(status);
    }
}

struct Card<'a> {
    key: &'a str,
    record: &'a DriveRecord,
    attached: Option<&'a str>,
    indexed: Option<(usize, u64)>,
    diff: Option<ContentDiff>,
    labels: &'a HashMap<&'a str, &'a str>,
    checking: bool,
    health_error: Option<&'a str>,
    any_check_running: bool,
    now: SystemTime,
}

impl Card<'_> {
    fn show(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let assessment = self.record.assessment();
        let accent = match assessment.as_ref().map(|a| a.status) {
            Some(HealthStatus::Failing) => DANGER,
            Some(HealthStatus::Caution) => CAUTION,
            _ => ui.visuals().widgets.noninteractive.bg_stroke.color,
        };
        Frame::group(ui.style())
            .stroke(egui::Stroke::new(1.0, accent))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.header(ui, assessment.as_ref().map(|a| a.status), actions);
                self.device(ui);
                ui.add_space(4.0);
                self.usage(ui);
                self.index(ui);
                self.twin(ui, actions);
                ui.add_space(4.0);
                self.health(ui);
                self.attention(ui);
            });
    }

    fn header(&self, ui: &mut Ui, status: Option<HealthStatus>, actions: &mut Vec<Action>) {
        let r = self.record;
        ui.horizontal(|ui| {
            ui.label(icons::ICON_HARD_DRIVE.rich_text().size(20.0));
            ui.label(RichText::new(&r.label).strong().size(16.0));
            match r.serial {
                Some(serial) => ui.weak(format!("Volume serial {}", format_serial(serial))),
                None => ui
                    .weak("Volume serial unknown")
                    .on_hover_text("Indexed by an older version; plug the drive in to record it."),
            };
            match self.attached {
                Some(letter) => {
                    ui.colored_label(
                        GOOD,
                        format!("{} Attached as {letter}", icons::ICON_USB.codepoint),
                    );
                }
                None => {
                    let seen = r
                        .last_seen
                        .map(|t| format!("Last seen {}", format_ago(t, self.now)))
                        .unwrap_or_else(|| format!("Last seen as {}", r.last_letter));
                    ui.colored_label(MUTED, seen);
                }
            }
            let (text, color) = match status {
                Some(HealthStatus::Good) => ("Healthy", GOOD),
                Some(HealthStatus::Caution) => ("Caution", CAUTION),
                Some(HealthStatus::Failing) => ("Failing", DANGER),
                None => ("Health unknown", MUTED),
            };
            ui.colored_label(
                color,
                format!("{} {text}", icons::ICON_MONITOR_HEART.codepoint),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button(format!("{} Forget...", icons::ICON_DELETE.codepoint))
                    .on_hover_text("Remove this drive from the list and its files from the index")
                    .clicked()
                {
                    actions.push(Action::AskForget(self.key.to_string()));
                }
                if let Some(letter) = self.attached {
                    if ui
                        .small_button(format!("{} Open", icons::ICON_FOLDER_OPEN.codepoint))
                        .clicked()
                    {
                        actions.push(Action::Open(letter.to_string()));
                    }
                    if ui
                        .add_enabled(
                            !self.any_check_running,
                            egui::Button::new(format!(
                                "{} Check health",
                                icons::ICON_MONITOR_HEART.codepoint
                            ))
                            .small(),
                        )
                        .on_hover_text(
                            "Read this drive's SMART data (asks for administrator rights)",
                        )
                        .clicked()
                    {
                        actions.push(Action::CheckHealth(vec![VolumeInfo {
                            drive_letter: letter.to_string(),
                            label: r.label.clone(),
                            serial: r.serial,
                        }]));
                    }
                }
                if self.checking {
                    ui.spinner();
                }
            });
        });
    }

    fn device(&self, ui: &mut Ui) {
        let Some(d) = &self.record.device else {
            return;
        };
        let mut parts = Vec::new();
        if !d.model.is_empty() {
            parts.push(d.model.clone());
        }
        if !d.serial.is_empty() {
            parts.push(format!("S/N {}", d.serial));
        }
        if !d.firmware.is_empty() {
            parts.push(format!("firmware {}", d.firmware));
        }
        if !d.bus.is_empty() {
            parts.push(format!("via {}", d.bus));
        }
        if !parts.is_empty() {
            ui.weak(parts.join("  ·  "));
        }
    }

    fn usage(&self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(icons::ICON_DATA_USAGE.rich_text());
            match self.record.usage {
                Some(usage) => {
                    ui.add(
                        egui::ProgressBar::new(usage.used_fraction())
                            .desired_width(180.0)
                            .text(format!("{:.0}% used", usage.used_fraction() * 100.0)),
                    );
                    ui.label(format!(
                        "{} of {} used, {} free",
                        format_size(usage.used(), DECIMAL),
                        format_size(usage.total, DECIMAL),
                        format_size(usage.free, DECIMAL),
                    ));
                    ui.weak(format!("as of {}", format_timestamp(usage.recorded_at)));
                }
                None => {
                    ui.weak(
                        "Usage not recorded yet (index or track the drive while it's attached)",
                    );
                }
            }
        });
    }

    fn index(&self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(icons::ICON_MANAGE_SEARCH.rich_text());
            match self.indexed {
                Some((files, bytes)) => {
                    ui.label(format!(
                        "{files} file(s), {} indexed",
                        format_size(bytes, DECIMAL)
                    ));
                    if let Some(t) = self.record.last_indexed {
                        ui.weak(format!("last indexed {}", format_ago(t, self.now)));
                    }
                }
                None => {
                    ui.weak("Not indexed (index it in the Reverse Search tab)");
                }
            }
        });
    }

    fn twin(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let r = self.record;
        ui.horizontal(|ui| {
            ui.label(icons::ICON_CONTENT_COPY.rich_text());
            ui.label("Twin:");
            let selected_text = match &r.twin {
                Some(twin) => self
                    .labels
                    .get(twin.as_str())
                    .copied()
                    .unwrap_or("(unknown)")
                    .to_string(),
                None => "None".to_string(),
            };
            egui::ComboBox::from_id_salt(("twin", self.key))
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(r.twin.is_none(), "None").clicked() {
                        actions.push(Action::SetTwin(self.key.to_string(), None));
                    }
                    let mut others: Vec<_> = self
                        .labels
                        .iter()
                        .filter(|(k, _)| **k != self.key)
                        .collect();
                    others.sort_by_key(|(k, label)| (**label, **k));
                    for (k, label) in others {
                        let selected = r.twin.as_deref() == Some(*k);
                        let serial = k.split('|').next().unwrap_or_default();
                        if ui
                            .selectable_label(selected, format!("{label}  ({serial})"))
                            .clicked()
                        {
                            actions
                                .push(Action::SetTwin(self.key.to_string(), Some(k.to_string())));
                        }
                    }
                });

            if r.twin.is_some() {
                match self.diff {
                    Some(d) if d.in_sync() => {
                        ui.colored_label(GOOD, "Same content on both, as of their last index");
                    }
                    Some(d) => {
                        let mut parts = Vec::new();
                        if d.only_a_files > 0 {
                            parts.push(format!(
                                "{} file(s) ({}) only on this drive",
                                d.only_a_files,
                                format_size(d.only_a_bytes, DECIMAL)
                            ));
                        }
                        if d.only_b_files > 0 {
                            parts.push(format!(
                                "{} file(s) ({}) only on its twin",
                                d.only_b_files,
                                format_size(d.only_b_bytes, DECIMAL)
                            ));
                        }
                        ui.colored_label(CAUTION, parts.join(", ")).on_hover_text(
                            "Compared by content hash from each drive's last index, so renamed \
                             or moved files still count as present on both.",
                        );
                    }
                    None => {
                        ui.weak("Index both drives to compare them");
                    }
                }
            }
        });
    }

    fn health(&self, ui: &mut Ui) {
        if let Some(err) = self.health_error {
            ui.colored_label(
                DANGER,
                format!("{} Health check failed: {err}", icons::ICON_ERROR.codepoint),
            );
        }
        let Some(latest) = self.record.health.last() else {
            return;
        };
        let attrs = &latest.attributes;
        let value = |id: u8| attrs.iter().find(|a| a.id == id).map(SmartAttribute::value);
        ui.horizontal_wrapped(|ui| {
            ui.label(icons::ICON_MONITOR_HEART.rich_text());
            let mut figure = |name: &str, v: Option<u64>, bad_if_nonzero: bool| {
                if let Some(v) = v {
                    let text = format!("{name}: {v}");
                    if bad_if_nonzero && v > 0 {
                        ui.colored_label(CAUTION, text);
                    } else {
                        ui.label(text);
                    }
                }
            };
            figure("Power-on hours", value(smart::POWER_ON_HOURS), false);
            figure("Power cycles", value(smart::POWER_CYCLES), false);
            figure("Reallocated", value(smart::REALLOCATED), true);
            figure("Pending", value(smart::PENDING), true);
            figure("Uncorrectable", value(smart::UNCORRECTABLE), true);
            figure("CRC errors", value(smart::CRC_ERRORS), false);
            if let Some(t) = smart::temperature(attrs) {
                ui.label(format!("{t} °C"));
            }
            ui.weak(format!("checked {}", format_ago(latest.at, self.now)));
        });

        egui::CollapsingHeader::new("SMART details")
            .id_salt(("smart", self.key))
            .show(ui, |ui| {
                attribute_table(ui, self.key, attrs);
                if self.record.health.len() > 1 {
                    ui.add_space(6.0);
                    ui.strong("History");
                    history_table(ui, self.key, self.record);
                }
            });
    }

    fn attention(&self, ui: &mut Ui) {
        let notes = attention(
            self.record,
            self.indexed.is_some(),
            self.attached.is_some(),
            self.now,
        );
        let failing = self.record.assessment().map(|a| a.status) == Some(HealthStatus::Failing);
        for note in notes {
            let color = if failing { DANGER } else { CAUTION };
            ui.colored_label(color, format!("{} {note}", icons::ICON_WARNING.codepoint));
        }
        if let Some(a) = self.record.assessment()
            && a.status == HealthStatus::Good
        {
            for note in a.notes {
                ui.weak(format!("{} {note}", icons::ICON_INFO.codepoint));
            }
        }
    }
}

fn attribute_table(ui: &mut Ui, key: &str, attrs: &[SmartAttribute]) {
    TableBuilder::new(ui)
        .id_salt(("smart_attrs", key))
        .striped(true)
        .vscroll(false)
        .column(Column::exact(40.0))
        .column(Column::remainder().at_least(200.0))
        .columns(Column::exact(64.0), 3)
        .column(Column::exact(110.0))
        .column(Column::exact(64.0))
        .header(18.0, |mut h| {
            for title in [
                "ID",
                "Attribute",
                "Value",
                "Worst",
                "Threshold",
                "Raw",
                "Type",
            ] {
                h.col(|ui| {
                    ui.strong(title);
                });
            }
        })
        .body(|mut body| {
            for a in attrs {
                body.row(18.0, |mut row| {
                    let color = if a.below_threshold() {
                        Some(if a.prefail { DANGER } else { CAUTION })
                    } else {
                        None
                    };
                    let cell = |ui: &mut Ui, text: String| match color {
                        Some(c) => ui.colored_label(c, text),
                        None => ui.label(text),
                    };
                    row.col(|ui| {
                        cell(ui, a.id.to_string());
                    });
                    row.col(|ui| {
                        cell(ui, a.name().to_string());
                    });
                    row.col(|ui| {
                        cell(ui, a.current.to_string());
                    });
                    row.col(|ui| {
                        cell(ui, a.worst.to_string());
                    });
                    row.col(|ui| {
                        cell(ui, a.threshold.to_string());
                    });
                    row.col(|ui| {
                        cell(ui, a.value().to_string());
                    });
                    row.col(|ui| {
                        ui.weak(if a.prefail { "Pre-fail" } else { "Old age" });
                    });
                });
            }
        });
}

/// One row per health check, newest first, with the counters that matter.
fn history_table(ui: &mut Ui, key: &str, record: &DriveRecord) {
    TableBuilder::new(ui)
        .id_salt(("smart_history", key))
        .striped(true)
        .vscroll(false)
        .column(Column::exact(130.0))
        .column(Column::exact(80.0))
        .columns(Column::exact(90.0), 4)
        .header(18.0, |mut h| {
            for title in [
                "Checked",
                "Status",
                "Reallocated",
                "Pending",
                "CRC errors",
                "Temp",
            ] {
                h.col(|ui| {
                    ui.strong(title);
                });
            }
        })
        .body(|mut body| {
            let samples = &record.health;
            for i in (0..samples.len()).rev().take(20) {
                let s = &samples[i];
                let previous = i.checked_sub(1).map(|p| samples[p].attributes.as_slice());
                let status = smart::assess(&s.attributes, previous).status;
                let value = |id: u8| {
                    s.attributes
                        .iter()
                        .find(|a| a.id == id)
                        .map_or("-".to_string(), |a| a.value().to_string())
                };
                body.row(18.0, |mut row| {
                    row.col(|ui| {
                        ui.label(format_timestamp(s.at));
                    });
                    row.col(|ui| {
                        let (text, color) = match status {
                            HealthStatus::Good => ("Healthy", GOOD),
                            HealthStatus::Caution => ("Caution", CAUTION),
                            HealthStatus::Failing => ("Failing", DANGER),
                        };
                        ui.colored_label(color, text);
                    });
                    row.col(|ui| {
                        ui.label(value(smart::REALLOCATED));
                    });
                    row.col(|ui| {
                        ui.label(value(smart::PENDING));
                    });
                    row.col(|ui| {
                        ui.label(value(smart::CRC_ERRORS));
                    });
                    row.col(|ui| {
                        ui.label(
                            smart::temperature(&s.attributes)
                                .map_or("-".to_string(), |t| format!("{t} °C")),
                        );
                    });
                });
            }
        });
}

fn show_untracked(app: &DupeApp, ui: &mut Ui, actions: &mut Vec<Action>) {
    let untracked = app.drives.untracked();
    if untracked.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.separator();
    ui.strong("Attached, not tracked");
    for drive in untracked {
        ui.horizontal(|ui| {
            if ui
                .small_button(format!("{} Track", icons::ICON_ADD.codepoint))
                .on_hover_text("Add it to the list above")
                .clicked()
            {
                actions.push(Action::Track(drive.clone()));
            }
            ui.label(icons::ICON_HARD_DRIVE.rich_text());
            ui.label(format!(
                "{} ({})",
                drive.volume.label, drive.volume.drive_letter
            ));
            if let Some(d) = &drive.device {
                ui.weak(format!("{}  ·  via {}", d.model, d.bus));
            }
        });
    }
}

fn show_forget_modal(app: &mut DupeApp, ui: &mut Ui) {
    let Some(key) = app.drives.pending_forget.clone() else {
        return;
    };
    let label = app
        .drives
        .registry
        .get(&key)
        .map_or(key.clone(), |r| r.label.clone());
    let files = app
        .reverse_search
        .db
        .volumes()
        .iter()
        .find(|v| v.key == key)
        .map_or(0, |v| v.files);

    let mut confirmed = false;
    let mut dismissed = false;
    let response = Modal::new(Id::new("forget_drive")).show(ui.ctx(), |ui| {
        ui.set_max_width(420.0);
        ui.heading(format!("{} Forget {label}?", icons::ICON_WARNING.codepoint));
        ui.add_space(4.0);
        ui.label(format!(
            "Its usage, twin pairing and health history are removed, along with its {files} \
             file(s) in the reverse-search index. Nothing on the drive itself is touched."
        ));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .button(RichText::new("Forget drive").color(DANGER))
                .clicked()
            {
                confirmed = true;
            }
            if ui.button("Keep it").clicked() {
                dismissed = true;
            }
        });
    });
    if response.should_close() {
        dismissed = true;
    }
    if confirmed {
        app.drives.pending_forget = None;
        app.drives.forget(&key, &mut app.reverse_search);
    } else if dismissed {
        app.drives.pending_forget = None;
    }
}
