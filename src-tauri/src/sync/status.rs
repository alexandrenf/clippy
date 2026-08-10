use image::{Rgba, RgbaImage};
use serde::Serialize;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncState {
    #[default]
    Idle,
    Syncing,
    Synced,
    WaitingForDevice,
}

pub struct SyncStatus(pub Mutex<SyncState>);

impl Default for SyncStatus {
    fn default() -> Self {
        Self(Mutex::new(SyncState::Idle))
    }
}

pub fn set(app: &AppHandle, status: &SyncStatus, state: SyncState) {
    let changed = status
        .0
        .lock()
        .map(|mut current| {
            if *current == state {
                false
            } else {
                *current = state;
                true
            }
        })
        .unwrap_or(false);
    if !changed {
        return;
    }

    if let Some(tray) = app.tray_by_id("clippy-tray") {
        let _ = tray.set_tooltip(Some(tooltip(state)));
        #[cfg(target_os = "macos")]
        if let Ok(icon) = status_icon(state) {
            let _ = tray.set_icon(Some(icon));
        }
    }
    let _ = app.emit("sync-state-changed", state);
}

pub fn get(status: &SyncStatus) -> SyncState {
    status
        .0
        .lock()
        .map(|value| *value)
        .unwrap_or(SyncState::Idle)
}

fn tooltip(state: SyncState) -> &'static str {
    match state {
        SyncState::Idle => "Clippy — sync is not configured",
        SyncState::Syncing => "Clippy — syncing",
        SyncState::Synced => "Clippy — synced",
        SyncState::WaitingForDevice => "Clippy — waiting for a device",
    }
}

#[cfg(target_os = "macos")]
fn status_icon(state: SyncState) -> tauri::Result<tauri::image::Image<'static>> {
    let source = image::load_from_memory(include_bytes!("../../icons/tray-icon.png"))?;
    let mut image = source.to_rgba8();
    match state {
        SyncState::Idle => {}
        SyncState::Synced => draw_filled_dot(&mut image),
        SyncState::Syncing => draw_sync_dots(&mut image),
        SyncState::WaitingForDevice => draw_ring(&mut image),
    }
    Ok(tauri::image::Image::new_owned(
        image.as_raw().clone(),
        image.width(),
        image.height(),
    ))
}

fn draw_filled_dot(image: &mut RgbaImage) {
    let radius = badge_radius(image);
    let center = badge_center(image, radius);
    paint_circle(image, center, radius, false);
}

fn draw_sync_dots(image: &mut RgbaImage) {
    let radius = (badge_radius(image) / 2).max(1);
    let (x, y) = badge_center(image, badge_radius(image));
    paint_circle(image, (x.saturating_sub(radius * 2), y), radius, false);
    paint_circle(image, (x.saturating_add(radius * 2), y), radius, false);
}

fn draw_ring(image: &mut RgbaImage) {
    let radius = badge_radius(image);
    paint_circle(image, badge_center(image, radius), radius, true);
}

fn badge_radius(image: &RgbaImage) -> u32 {
    (image.width().min(image.height()) / 9).max(1)
}

fn badge_center(image: &RgbaImage, radius: u32) -> (u32, u32) {
    (
        image.width().saturating_sub(radius + 1),
        image.height().saturating_sub(radius + 1),
    )
}

fn paint_circle(image: &mut RgbaImage, center: (u32, u32), radius: u32, ring: bool) {
    let outer = (radius * radius) as i64;
    let inner_radius = radius.saturating_sub((radius / 2).max(1));
    let inner = (inner_radius * inner_radius) as i64;
    let min_x = center.0.saturating_sub(radius);
    let max_x = (center.0 + radius).min(image.width().saturating_sub(1));
    let min_y = center.1.saturating_sub(radius);
    let max_y = (center.1 + radius).min(image.height().saturating_sub(1));
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let dx = x as i64 - center.0 as i64;
            let dy = y as i64 - center.1 as i64;
            let distance = dx * dx + dy * dy;
            if distance <= outer && (!ring || distance >= inner) {
                // Template icons use alpha as the mask. RGB is intentionally
                // black so macOS can render the correct menu-bar appearance.
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_variants_change_the_template_mask() {
        let base = image::load_from_memory(include_bytes!("../../icons/tray-icon.png"))
            .unwrap()
            .to_rgba8();
        let mut synced = base.clone();
        draw_filled_dot(&mut synced);
        let mut waiting = base.clone();
        draw_ring(&mut waiting);
        assert_ne!(base.as_raw(), synced.as_raw());
        assert_ne!(synced.as_raw(), waiting.as_raw());
    }
}
