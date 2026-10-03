use crate::app::DupeApp;
use crate::disk_usage::{DirNode, DiskUsageState, Row};
use crate::ui::dialogs::PickPurpose;
use egui::{RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_material_icons::icons;
use humansize::{DECIMAL, format_size};
use std::path::PathBuf;

const ROW_HEIGHT: f32 = 22.0;
const INDENT: f32 = 16.0;

pub fn show(app: &mut DupeApp, ui: &mut Ui) {
    ui.heading("Disk Usage");
    ui.label(
        "See how much space every folder and subfolder of a folder takes, largest first. \
         Expand a folder to see what's inside it.",
    );
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        ui.label(icons::ICON_FOLDER_OPEN.rich_text());
        ui.strong("Folder");
        if ui.button("Choose...").clicked() {
            app.start_pick(PickPurpose::DiskUsageRoot);
        }
        match &app.disk_usage.root {
            Some(p) => ui.label(p.display().to_string()),
            None => ui.weak("no folder chosen"),
        };
    });
    ui.add_space(4.0);

    let state = &mut app.disk_usage;
    ui.horizontal(|ui| {
        let scanning = state.is_scanning();
        if ui
            .add_enabled(
                state.root.is_some() && !scanning,
                egui::Button::new(RichText::from(format!(
                    "{} Rescan",
                    icons::ICON_REFRESH.codepoint
                ))),
            )
            .clicked()
        {
            state.rescan();
        }
        let has_tree = state.tree.is_some();
        if ui
            .add_enabled(
                has_tree,
                egui::Button::new(RichText::from(format!(
                    "{} Expand all",
                    icons::ICON_UNFOLD_MORE.codepoint
                ))),
            )
            .clicked()
        {
            state.expand_all();
        }
        if ui
            .add_enabled(
                has_tree,
                egui::Button::new(RichText::from(format!(
                    "{} Collapse all",
                    icons::ICON_UNFOLD_LESS.codepoint
                ))),
            )
            .clicked()
        {
            state.collapse_all();
        }
        if scanning {
            if ui
                .button(RichText::from(format!(
                    "{} Stop",
                    icons::ICON_STOP.codepoint
                )))
                .clicked()
            {
                state.cancel();
            }
            ui.spinner();
            let (files, bytes, _) = &state.progress;
            ui.label(format!(
                "Measuring... {files} file(s), {} so far",
                format_size(*bytes, DECIMAL)
            ));
        }
    });
    if let (true, Some(current)) = (state.is_scanning(), &state.progress.2) {
        ui.add(
            egui::Label::new(RichText::new(current.display().to_string()).monospace()).truncate(),
        );
    }
    if let Some(msg) = &state.status {
        ui.label(msg);
    }
    ui.separator();

    let mut open_path = None;
    show_tree(state, ui, &mut open_path);
    if let Some(path) = open_path
        && let Err(err) = open::that(&path)
    {
        app.disk_usage.status = Some(format!("Couldn't open {}: {err}", path.display()));
    }
}

fn show_tree(state: &mut DiskUsageState, ui: &mut Ui, open_path: &mut Option<PathBuf>) {
    let Some(tree) = &state.tree else {
        return;
    };
    let mut toggle = None;
    TableBuilder::new(ui)
        .id_salt("disk_usage_tree")
        .striped(true)
        .column(Column::remainder().at_least(260.0).resizable(true))
        .column(Column::auto().at_least(90.0))
        .column(Column::exact(140.0))
        .column(Column::auto().at_least(70.0))
        .column(Column::auto().at_least(70.0))
        .column(Column::exact(28.0))
        .header(20.0, |mut header| {
            for title in ["Folder", "Size", "Share of parent", "Files", "Folders", ""] {
                header.col(|ui| {
                    ui.strong(title);
                });
            }
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, state.rows().len(), |mut row| {
                let r = state.rows()[row.index()];
                let (node, depth, size, files) = match r {
                    Row::Dir(idx) => {
                        let n = &tree.nodes[idx];
                        (n, n.depth, n.size, n.file_count)
                    }
                    Row::Files(idx) => {
                        let n = &tree.nodes[idx];
                        (n, n.depth + 1, n.own_size, n.own_file_count)
                    }
                };
                let parent_size = match r {
                    Row::Dir(idx) => tree.nodes[idx].parent.map(|p| tree.nodes[p].size),
                    Row::Files(_) => Some(node.size),
                };
                row.col(|ui| {
                    ui.add_space(depth as f32 * INDENT);
                    match r {
                        Row::Dir(idx) => {
                            let chevron = if node.children.is_empty() {
                                "  "
                            } else if state.is_expanded(idx) {
                                icons::ICON_EXPAND_MORE.codepoint
                            } else {
                                icons::ICON_CHEVRON_RIGHT.codepoint
                            };
                            let label =
                                format!("{chevron} {} {}", icons::ICON_FOLDER.codepoint, node.name);
                            let response = ui
                                .add(egui::Button::new(label).frame(false).truncate())
                                .on_hover_text(node.path.display().to_string());
                            if response.clicked() && !node.children.is_empty() {
                                toggle = Some(idx);
                            }
                        }
                        Row::Files(_) => {
                            ui.weak(RichText::new("(files in this folder)").italics());
                        }
                    }
                });
                row.col(|ui| {
                    ui.label(format_size(size, DECIMAL));
                });
                row.col(|ui| {
                    let share = share(size, parent_size);
                    ui.add(
                        egui::ProgressBar::new(share)
                            .text(format!("{:.1} %", share * 100.0))
                            .desired_height(ROW_HEIGHT - 6.0),
                    );
                });
                row.col(|ui| {
                    ui.label(files.to_string());
                });
                row.col(|ui| {
                    if let Row::Dir(_) = r {
                        ui.label(node.dir_count.to_string());
                    }
                });
                row.col(|ui| {
                    if let Row::Dir(_) = r {
                        open_button(node, ui, open_path);
                    }
                });
            });
        });
    if let Some(idx) = toggle {
        state.toggle(idx);
    }
}

fn open_button(node: &DirNode, ui: &mut Ui, open_path: &mut Option<PathBuf>) {
    if ui
        .add(egui::Button::new(icons::ICON_OPEN_IN_NEW.rich_text()).frame(false))
        .on_hover_text("Open in Explorer")
        .clicked()
    {
        *open_path = Some(node.path.clone());
    }
}

/// `size` as a fraction of `parent`; the top folder (no parent) is 100 %.
fn share(size: u64, parent: Option<u64>) -> f32 {
    match parent {
        None => 1.0,
        Some(0) => 0.0,
        Some(p) => (size as f64 / p as f64) as f32,
    }
}
