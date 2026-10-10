//! Searchable, docked plug-in browser. Metadata comes from SoundCraft's registries.

use std::collections::BTreeMap;

use crate::SoundApp;
use egui::{Sense, Ui};
use serde_json::json;
use soundcraft_model::{TrackId, TrackKind};

#[derive(Debug, Clone)]
pub struct PluginDrag {
    pub id: String,
    pub instrument: bool,
}

#[derive(Debug, Clone)]
pub struct PluginEntry {
    id: String,
    name: String,
    vendor: String,
    category: String,
    format: &'static str,
    instrument: bool,
}

pub fn entries(instrument: bool) -> Vec<PluginEntry> {
    let mut items: Vec<PluginEntry> = soundcraft_dsp::plugins().iter().filter(|p| p.is_instrument == instrument)
        .map(|p| PluginEntry {
            id: p.id.to_string(),
            name: p.name.to_string(),
            vendor: "SoundCraft".to_string(),
            category: format!("{:?}", p.category),
            format: "Built-in",
            instrument: p.is_instrument,
        }).collect();
    items.extend(soundcraft_clap_host::scan().into_iter().filter(|p| p.is_instrument == instrument)
        .map(|p| PluginEntry { id: p.id, name: p.name, vendor: p.vendor, category: format!("{:?}", p.category), format: "CLAP", instrument }));
    items.extend(soundcraft_vst3_host::scan().into_iter().filter(|p| p.is_instrument == instrument)
        .map(|p| PluginEntry { id: p.id, name: p.name, vendor: p.vendor, category: format!("{:?}", p.category), format: "VST3", instrument }));
    items.extend(soundcraft_au_host::scan().into_iter().filter(|p| p.is_instrument == instrument)
        .map(|p| PluginEntry { id: p.id, name: p.name, vendor: p.vendor, category: format!("{:?}", p.category), format: "AU", instrument }));
    items.sort_by_cached_key(|p| (p.name.to_lowercase(), p.vendor.to_lowercase(), p.id.clone()));
    items
}

fn label(ui: &mut Ui, p: &PluginEntry, selected: bool) -> egui::Response {
    ui.add(egui::Button::new(format!("{}  ({})", p.name, p.format))
        .selected(selected).sense(Sense::click_and_drag()))
}

fn plugin_row(ui: &mut Ui, p: &PluginEntry, favorites: &mut Vec<String>) -> bool {
    let favorite = favorites.contains(&p.id);
    let resp = label(ui, p, favorite);
    resp.dnd_set_drag_payload(PluginDrag { id: p.id.clone(), instrument: p.instrument });
    resp.context_menu(|ui| {
        if ui.button(if favorite { "Remove from Favorites" } else { "Add to Favorites" }).clicked() {
            if favorite { favorites.retain(|id| id != &p.id); }
            else { favorites.push(p.id.clone()); }
            ui.close();
        }
    });
    resp.clicked()
}

/// A common searchable list, also used in the small insert and instrument pickers.
/// The caller owns search/sort/favorite state; this function only reports which plug-in was clicked.
pub fn plugin_list(ui: &mut Ui, items: &[PluginEntry], query: &str, sort: &str, favorites: &mut Vec<String>) -> Option<String> {
    let needle = query.trim().to_lowercase();
    let matches: Vec<&PluginEntry> = items.iter().filter(|p| {
        (sort != "Favorites" || favorites.contains(&p.id))
            && (needle.is_empty() || p.name.to_lowercase().contains(&needle)
            || p.vendor.to_lowercase().contains(&needle)
            || p.category.to_lowercase().contains(&needle)
            || p.format.to_lowercase().contains(&needle))
    }).collect();
    let mut picked = None;
    if sort == "Flat" || sort == "Favorites" {
        for p in matches {
            if plugin_row(ui, p, favorites) { picked = Some(p.id.clone()); }
        }
    } else {
        let mut groups: BTreeMap<String, Vec<&PluginEntry>> = BTreeMap::new();
        for p in matches {
            let key = match sort {
                "Vendor" => if p.vendor.is_empty() { "Unknown Vendor".to_string() } else { p.vendor.clone() },
                "Type" => p.format.to_string(),
                _ => p.category.clone(),
            };
            groups.entry(key).or_default().push(p);
        }
        for (group, plugins) in groups {
            egui::CollapsingHeader::new(format!("{} ({})", group, plugins.len()))
                .id_salt(("browser_group", sort, &group))
                .show(ui, |ui| {
                    for p in plugins {
                        if plugin_row(ui, p, favorites) { picked = Some(p.id.clone()); }
                    }
                });
        }
    }
    picked
}

pub fn picker(ui: &mut Ui, instrument: bool) -> Option<String> {
    let id = ui.id().with(("plugin_picker", instrument));
    let mut query: String = ui.ctx().data(|d| d.get_temp(id)).unwrap_or_default();
    ui.add(egui::TextEdit::singleline(&mut query).hint_text("Search name, vendor, category…"));
    ui.ctx().data_mut(|d| d.insert_temp(id, query.clone()));
    let entries = entries(instrument);
    let mut favorites = Vec::new();
    egui::ScrollArea::vertical().id_salt(("picker_list", instrument)).max_height(340.0).show(ui, |ui| {
        plugin_list(ui, &entries, &query, "Folder", &mut favorites)
    }).inner
}

pub fn show(app: &mut SoundApp, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.heading("Browser");
        if ui.small_button("×").clicked() { app.ui.show_browser = false; }
    });
    ui.horizontal(|ui| {
        if ui.selectable_label(!app.ui.browser_instruments && !app.ui.browser_clips, "Effects").clicked() {
            app.ui.browser_instruments = false;
            app.ui.browser_clips = false;
        }
        if ui.selectable_label(app.ui.browser_instruments && !app.ui.browser_clips, "Instruments").clicked() {
            app.ui.browser_instruments = true;
            app.ui.browser_clips = false;
        }
        if ui.selectable_label(app.ui.browser_clips, "Clips").clicked() {
            app.ui.browser_clips = true;
        }
    });
    if app.ui.browser_clips {
        crate::panels::clip_list(app, ui);
        return;
    }
    ui.add(egui::TextEdit::singleline(&mut app.ui.browser_search)
        .hint_text("Search plug-ins, vendors, categories…").desired_width(f32::INFINITY));
    egui::ComboBox::from_id_salt("browser_sort")
        .selected_text(&app.ui.browser_sort)
        .show_ui(ui, |ui| {
            for s in ["Flat", "Folder", "Vendor", "Type", "Favorites"] {
                ui.selectable_value(&mut app.ui.browser_sort, s.to_string(), s);
            }
        });
    let entries = entries(app.ui.browser_instruments);
    ui.label(format!("{} plug-ins · drag onto track or insert", entries.len()));
    let picked = egui::ScrollArea::vertical().id_salt("plugin_browser_list")
        .auto_shrink([false, false]).show(ui, |ui| {
            plugin_list(ui, &entries, &app.ui.browser_search, &app.ui.browser_sort, &mut app.ui.browser_favorites)
        }).inner;
    if let Some(id) = picked {
        let selected = app.engine.session().edit.selected_tracks.first().copied();
        if let Some(track) = selected {
            insert_on_track(app, track, &PluginDrag { id, instrument: app.ui.browser_instruments }, None);
        } else {
            app.ui.status = "Select a track or drag a plug-in onto a channel".into();
        }
    }
}

pub fn insert_on_track(app: &mut SoundApp, id: TrackId, plugin: &PluginDrag, slot: Option<usize>) {
    let kind = app.engine.session().track(id).map(|t| t.kind);
    if plugin.instrument {
        if kind == Some(TrackKind::Instrument) {
            let _ = app.run("mix.instrument", json!({"track": id.0, "plugin": plugin.id}));
        } else {
            app.ui.status = "Instruments need an instrument track".into();
        }
    } else {
        let mut p = json!({"track": id.0, "plugin": plugin.id});
        if let Some(slot) = slot { p["slot"] = json!(slot); }
        let _ = app.run("mix.insert", p);
    }
}
