//! Meeting companion window lifecycle and monitor-aware position persistence.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::Mutex;
use tauri::{
    AppHandle, Emitter, Manager, Monitor, PhysicalPosition, PhysicalSize, State, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

use crate::coordinator::Coordinator;
use crate::types::MeetingCompanionPosition;

const WINDOW_LABEL: &str = "meeting-companion";
const WINDOW_WIDTH: f64 = 240.0;
const WINDOW_HEIGHT: f64 = 232.0;
const EDGE_MARGIN: f64 = 16.0;
const DRAG_SETTLE_DELAY: Duration = Duration::from_millis(350);
const FAILED_DISMISS_DELAY: Duration = Duration::from_secs(3);
const COMPLETED_DISMISS_FALLBACK_DELAY: Duration = Duration::from_secs(9);

static LIFECYCLE: OnceLock<Mutex<LifecycleState>> = OnceLock::new();
static WINDOW_CREATION: OnceLock<Mutex<()>> = OnceLock::new();
static AUDIO_LEVEL_REPORTING_ENABLED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Default)]
struct LifecycleState {
    active_meeting_id: Option<String>,
    manually_hidden_meeting_id: Option<String>,
    window_creation_in_progress: bool,
    drag_active: bool,
    drag_epoch: u64,
}

impl LifecycleState {
    fn meeting_started(&mut self, meeting_id: &str, enabled: bool) -> bool {
        if self.active_meeting_id.as_deref() != Some(meeting_id) {
            self.manually_hidden_meeting_id = None;
        }
        self.active_meeting_id = Some(meeting_id.to_string());
        enabled && self.manually_hidden_meeting_id.as_deref() != Some(meeting_id)
    }

    fn manual_show(&mut self, meeting_id: &str) {
        self.active_meeting_id = Some(meeting_id.to_string());
        if self.manually_hidden_meeting_id.as_deref() == Some(meeting_id) {
            self.manually_hidden_meeting_id = None;
        }
    }

    fn manual_hide(&mut self) {
        self.manually_hidden_meeting_id = self.active_meeting_id.clone();
    }

    fn disable(&mut self) {
        self.manual_hide();
        self.drag_active = false;
        self.drag_epoch = self.drag_epoch.wrapping_add(1);
    }

    fn completion_finished(&mut self, meeting_id: &str) -> bool {
        if self.active_meeting_id.as_deref() != Some(meeting_id) {
            return false;
        }
        self.active_meeting_id = None;
        self.manually_hidden_meeting_id = None;
        self.drag_active = false;
        self.drag_epoch = self.drag_epoch.wrapping_add(1);
        true
    }

    fn hidden_failure_finished(&mut self, meeting_id: &str) -> bool {
        if self.manually_hidden_meeting_id.as_deref() != Some(meeting_id) {
            return false;
        }
        self.completion_finished(meeting_id)
    }

    fn claim_window_creation(&mut self, window_exists: bool) -> bool {
        if window_exists || self.window_creation_in_progress {
            return false;
        }
        self.window_creation_in_progress = true;
        true
    }

    fn finish_window_creation(&mut self) {
        self.window_creation_in_progress = false;
    }

    fn begin_drag(&mut self) -> u64 {
        self.drag_active = true;
        self.drag_epoch = self.drag_epoch.wrapping_add(1);
        self.drag_epoch
    }

    fn cancel_drag(&mut self, epoch: u64) {
        if self.drag_epoch == epoch {
            self.drag_active = false;
        }
    }

    fn note_drag_move(&mut self) -> Option<u64> {
        if !self.drag_active {
            return None;
        }
        self.drag_epoch = self.drag_epoch.wrapping_add(1);
        Some(self.drag_epoch)
    }

    fn finish_drag_if_idle(&mut self, epoch: u64) -> bool {
        if !self.drag_active || self.drag_epoch != epoch {
            return false;
        }
        self.drag_active = false;
        true
    }

    fn drag_matches(&self, epoch: u64) -> bool {
        self.drag_active && self.drag_epoch == epoch
    }

    fn finish_drag_now(&mut self) -> bool {
        if !self.drag_active {
            return false;
        }
        self.drag_active = false;
        self.drag_epoch = self.drag_epoch.wrapping_add(1);
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkArea {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, PartialEq)]
struct MonitorGeometry {
    id: String,
    work_area: WorkArea,
    scale_factor: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Placement {
    position: PhysicalPosition<i32>,
    monitor_id: String,
}

fn lifecycle() -> &'static Mutex<LifecycleState> {
    LIFECYCLE.get_or_init(|| Mutex::new(LifecycleState::default()))
}

fn window_creation_lock() -> &'static Mutex<()> {
    WINDOW_CREATION.get_or_init(|| Mutex::new(()))
}

pub(crate) fn meeting_started(app: &AppHandle, meeting_id: &str, enabled: bool) {
    let should_show = lifecycle().lock().meeting_started(meeting_id, enabled);
    if should_show {
        if let Err(error) = show_window(app) {
            log::warn!("[meeting-companion] automatic show failed: {error}");
        }
    }
}

pub(crate) fn audio_level_reporting_enabled() -> bool {
    AUDIO_LEVEL_REPORTING_ENABLED.load(Ordering::Relaxed)
}

pub(crate) fn schedule_completed_fallback_dismissal(app: &AppHandle, meeting_id: &str) {
    schedule_terminal_dismissal(app, meeting_id, COMPLETED_DISMISS_FALLBACK_DELAY);
}

pub(crate) fn schedule_failed_dismissal(app: &AppHandle, meeting_id: &str) {
    if lifecycle().lock().hidden_failure_finished(meeting_id) {
        destroy_window(app);
        return;
    }
    schedule_terminal_dismissal(app, meeting_id, FAILED_DISMISS_DELAY);
}

fn schedule_terminal_dismissal(app: &AppHandle, meeting_id: &str, delay: Duration) {
    if lifecycle().lock().active_meeting_id.as_deref() != Some(meeting_id) {
        return;
    }
    let app = app.clone();
    let meeting_id = meeting_id.to_string();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(delay).await;
        if lifecycle().lock().completion_finished(&meeting_id) {
            destroy_window(&app);
        }
    });
}

pub(crate) fn setting_disabled(app: &AppHandle) {
    lifecycle().lock().disable();
    destroy_window(app);
}

#[tauri::command]
pub fn show_meeting_companion(
    app: AppHandle,
    coord: State<'_, Arc<Coordinator>>,
) -> Result<(), String> {
    let prefs = coord.prefs().get();
    if !prefs.meeting_companion_enabled {
        return Err("meeting companion is disabled".to_string());
    }
    let active = coord
        .active_meeting_recording()?
        .ok_or_else(|| "no active meeting".to_string())?;
    lifecycle().lock().manual_show(&active.meeting.id);
    show_window(&app)
}

#[tauri::command]
pub fn hide_meeting_companion(app: AppHandle) -> Result<(), String> {
    hide_window(&app)
}

#[tauri::command]
pub fn start_meeting_companion_drag(
    app: AppHandle,
    coord: State<'_, Arc<Coordinator>>,
) -> Result<bool, String> {
    if !drag_allowed(coord.prefs().get().meeting_companion_position_locked) {
        return Ok(false);
    }
    let window = app
        .get_webview_window(WINDOW_LABEL)
        .ok_or_else(|| "meeting companion window is not available".to_string())?;
    let epoch = lifecycle().lock().begin_drag();
    if let Err(error) = window.start_dragging() {
        lifecycle().lock().cancel_drag(epoch);
        return Err(error.to_string());
    }
    schedule_drag_settle_epoch(&app, epoch);
    Ok(true)
}

#[tauri::command]
pub fn save_meeting_companion_position(app: AppHandle) -> Result<(), String> {
    if lifecycle().lock().finish_drag_now() {
        settle_and_persist_position(&app)?;
    }
    Ok(())
}

#[tauri::command]
pub fn dismiss_completed_meeting_companion(
    app: AppHandle,
    meeting_id: String,
) -> Result<bool, String> {
    if meeting_id.trim().is_empty() {
        return Err("meeting id is required".to_string());
    }
    let should_destroy = lifecycle().lock().completion_finished(&meeting_id);
    if should_destroy {
        destroy_window(&app);
    }
    Ok(should_destroy)
}

#[tauri::command]
pub fn set_meeting_companion_position_locked(
    app: AppHandle,
    window: WebviewWindow,
    coord: State<'_, Arc<Coordinator>>,
    locked: bool,
) -> Result<bool, String> {
    ensure_companion_invoker(window.label())?;
    let prefs = coord
        .prefs()
        .update(|prefs| prefs.meeting_companion_position_locked = locked)
        .map_err(|error| error.to_string())?;
    let _ = app.emit("prefs:changed", &prefs);
    Ok(prefs.meeting_companion_position_locked)
}

#[tauri::command]
pub fn open_meeting_from_companion(
    app: AppHandle,
    window: WebviewWindow,
    meeting_id: String,
) -> Result<(), String> {
    ensure_companion_invoker(window.label())?;
    let meeting_id = meeting_id.trim();
    if meeting_id.is_empty() {
        return Err("meeting id is required".to_string());
    }
    if lifecycle().lock().active_meeting_id.as_deref() != Some(meeting_id) {
        return Err("meeting is no longer current".to_string());
    }
    crate::show_main_window(&app);
    app.emit_to(
        "main",
        "meeting-companion:open-meeting",
        serde_json::json!({ "meetingId": meeting_id }),
    )
    .map_err(|error| error.to_string())
}

fn ensure_companion_invoker(window_label: &str) -> Result<(), String> {
    if window_label == WINDOW_LABEL {
        Ok(())
    } else {
        Err("command is only allowed from the meeting companion window".to_string())
    }
}

fn show_window(app: &AppHandle) -> Result<(), String> {
    let window = ensure_window(app)?;
    if let Err(error) = restore_window_position(app, &window) {
        log::warn!("[meeting-companion] position restore failed: {error}");
    }
    window.show().map_err(|error| error.to_string())?;
    if let Some(meeting_id) = lifecycle().lock().active_meeting_id.clone() {
        let _ = window.emit(
            "meeting-companion:show",
            serde_json::json!({ "meetingId": meeting_id }),
        );
    }
    AUDIO_LEVEL_REPORTING_ENABLED.store(true, Ordering::Relaxed);
    Ok(())
}

fn hide_window(app: &AppHandle) -> Result<(), String> {
    AUDIO_LEVEL_REPORTING_ENABLED.store(false, Ordering::Relaxed);
    lifecycle().lock().manual_hide();
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        window.hide().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn destroy_window(app: &AppHandle) {
    AUDIO_LEVEL_REPORTING_ENABLED.store(false, Ordering::Relaxed);
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        if let Err(error) = window.destroy() {
            log::warn!("[meeting-companion] destroy failed: {error}");
        }
    }
}

fn ensure_window(app: &AppHandle) -> Result<WebviewWindow, String> {
    let _creation_guard = window_creation_lock().lock();
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        return Ok(window);
    }

    let claimed = lifecycle().lock().claim_window_creation(false);
    debug_assert!(claimed, "window creation lock must serialize claims");
    let built = WebviewWindowBuilder::new(
        app,
        WINDOW_LABEL,
        WebviewUrl::App("index.html?window=meeting-companion".into()),
    )
    .title("OpenLess Meeting Companion")
    .inner_size(WINDOW_WIDTH, WINDOW_HEIGHT)
    .decorations(false)
    .transparent(true)
    .shadow(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .focused(false)
    .visible(false)
    .accept_first_mouse(true)
    .build();
    lifecycle().lock().finish_window_creation();

    let window = built.map_err(|error| error.to_string())?;
    register_window_events(app, &window);
    Ok(window)
}

fn register_window_events(app: &AppHandle, window: &WebviewWindow) {
    let app = app.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            if let Err(error) = hide_window(&app) {
                log::warn!("[meeting-companion] close-to-hide failed: {error}");
            }
        }
        WindowEvent::Moved(_) => schedule_drag_settle(&app),
        _ => {}
    });
}

fn schedule_drag_settle(app: &AppHandle) {
    let Some(epoch) = lifecycle().lock().note_drag_move() else {
        return;
    };
    schedule_drag_settle_epoch(app, epoch);
}

fn schedule_drag_settle_epoch(app: &AppHandle, epoch: u64) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(DRAG_SETTLE_DELAY).await;
        if primary_mouse_button_pressed() {
            if lifecycle().lock().drag_matches(epoch) {
                schedule_drag_settle_epoch(&app, epoch);
            }
            return;
        }
        if lifecycle().lock().finish_drag_if_idle(epoch) {
            if let Err(error) = settle_and_persist_position(&app) {
                log::warn!("[meeting-companion] drag position save failed: {error}");
            }
        }
    });
}

fn drag_allowed(position_locked: bool) -> bool {
    !position_locked
}

#[cfg(target_os = "windows")]
fn primary_mouse_button_pressed() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};

    unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) < 0 }
}

#[cfg(not(target_os = "windows"))]
fn primary_mouse_button_pressed() -> bool {
    false
}

fn restore_window_position(app: &AppHandle, window: &WebviewWindow) -> Result<(), String> {
    let monitors = monitor_geometries(app)?;
    let cursor_monitor_id = cursor_monitor_id(app);
    let primary_monitor_id = primary_monitor_id(app);
    let saved = app
        .state::<Arc<Coordinator>>()
        .prefs()
        .get()
        .meeting_companion_position;
    let placement = restore_placement(
        saved.as_ref(),
        &monitors,
        cursor_monitor_id.as_deref(),
        primary_monitor_id.as_deref(),
    )
    .ok_or_else(|| "no monitor is available".to_string())?;
    window
        .set_position(placement.position)
        .map_err(|error| error.to_string())?;
    if let Err(error) = persist_position(app, placement) {
        log::warn!("[meeting-companion] restored position save failed: {error}");
    }
    Ok(())
}

fn settle_and_persist_position(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(WINDOW_LABEL)
        .ok_or_else(|| "meeting companion window is not available".to_string())?;
    let position = window.outer_position().map_err(|error| error.to_string())?;
    let size = window.outer_size().map_err(|error| error.to_string())?;
    let monitors = monitor_geometries(app)?;
    let cursor_monitor_id = cursor_monitor_id(app);
    let primary_monitor_id = primary_monitor_id(app);
    let monitor = monitor_for_window(
        &monitors,
        position,
        size,
        cursor_monitor_id.as_deref(),
        primary_monitor_id.as_deref(),
    )
    .ok_or_else(|| "no monitor is available".to_string())?;
    let settled = snap_and_clamp_position(position, size, monitor);
    if settled != position {
        window
            .set_position(settled)
            .map_err(|error| error.to_string())?;
    }
    persist_position(
        app,
        Placement {
            position: settled,
            monitor_id: monitor.id.clone(),
        },
    )
}

fn persist_position(app: &AppHandle, placement: Placement) -> Result<(), String> {
    let coordinator = app.state::<Arc<Coordinator>>();
    let next = MeetingCompanionPosition {
        x: placement.position.x,
        y: placement.position.y,
        monitor_id: Some(placement.monitor_id),
    };
    if coordinator
        .prefs()
        .get()
        .meeting_companion_position
        .as_ref()
        == Some(&next)
    {
        return Ok(());
    }
    let prefs = coordinator
        .prefs()
        .update(|prefs| prefs.meeting_companion_position = Some(next))
        .map_err(|error| error.to_string())?;
    let _ = app.emit("prefs:changed", &prefs);
    Ok(())
}

fn monitor_geometries(app: &AppHandle) -> Result<Vec<MonitorGeometry>, String> {
    app.available_monitors()
        .map_err(|error| error.to_string())
        .map(|monitors| monitors.iter().map(MonitorGeometry::from).collect())
}

fn cursor_monitor_id(app: &AppHandle) -> Option<String> {
    let cursor = app.cursor_position().ok()?;
    app.monitor_from_point(cursor.x, cursor.y)
        .ok()
        .flatten()
        .map(|monitor| monitor_identifier(&monitor))
}

fn primary_monitor_id(app: &AppHandle) -> Option<String> {
    app.primary_monitor()
        .ok()
        .flatten()
        .map(|monitor| monitor_identifier(&monitor))
}

impl From<&Monitor> for MonitorGeometry {
    fn from(monitor: &Monitor) -> Self {
        let work_area = monitor.work_area();
        Self {
            id: monitor_identifier(monitor),
            work_area: WorkArea {
                x: work_area.position.x,
                y: work_area.position.y,
                width: work_area.size.width,
                height: work_area.size.height,
            },
            scale_factor: monitor.scale_factor(),
        }
    }
}

fn monitor_identifier(monitor: &Monitor) -> String {
    monitor
        .name()
        .filter(|name| !name.trim().is_empty())
        .map(|name| format!("name:{name}"))
        .unwrap_or_else(|| {
            let position = monitor.position();
            let size = monitor.size();
            format!(
                "geometry:{}:{}:{}:{}",
                position.x, position.y, size.width, size.height
            )
        })
}

fn restore_placement(
    saved: Option<&MeetingCompanionPosition>,
    monitors: &[MonitorGeometry],
    cursor_monitor_id: Option<&str>,
    primary_monitor_id: Option<&str>,
) -> Option<Placement> {
    if let Some(saved) = saved {
        if let Some(saved_monitor_id) = saved.monitor_id.as_deref() {
            if let Some(monitor) = monitors.iter().find(|item| item.id == saved_monitor_id) {
                return Some(Placement {
                    position: clamp_position(
                        PhysicalPosition::new(saved.x, saved.y),
                        window_size_for_monitor(monitor),
                        monitor.work_area,
                    ),
                    monitor_id: monitor.id.clone(),
                });
            }
        } else if let Some(monitor) =
            monitor_containing_point(monitors, PhysicalPosition::new(saved.x, saved.y))
        {
            return Some(Placement {
                position: clamp_position(
                    PhysicalPosition::new(saved.x, saved.y),
                    window_size_for_monitor(monitor),
                    monitor.work_area,
                ),
                monitor_id: monitor.id.clone(),
            });
        }
    }

    let fallback = monitor_by_preference(monitors, cursor_monitor_id, primary_monitor_id)?;
    Some(Placement {
        position: default_position(fallback),
        monitor_id: fallback.id.clone(),
    })
}

fn monitor_for_window<'a>(
    monitors: &'a [MonitorGeometry],
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    cursor_monitor_id: Option<&str>,
    primary_monitor_id: Option<&str>,
) -> Option<&'a MonitorGeometry> {
    let intersecting = monitors
        .iter()
        .map(|monitor| {
            (
                monitor,
                intersection_area(position, size, monitor.work_area),
            )
        })
        .max_by_key(|(_, area)| *area)
        .filter(|(_, area)| *area > 0)
        .map(|(monitor, _)| monitor);
    intersecting.or_else(|| monitor_by_preference(monitors, cursor_monitor_id, primary_monitor_id))
}

fn monitor_by_preference<'a>(
    monitors: &'a [MonitorGeometry],
    cursor_monitor_id: Option<&str>,
    primary_monitor_id: Option<&str>,
) -> Option<&'a MonitorGeometry> {
    cursor_monitor_id
        .and_then(|id| monitors.iter().find(|monitor| monitor.id == id))
        .or_else(|| {
            primary_monitor_id.and_then(|id| monitors.iter().find(|monitor| monitor.id == id))
        })
        .or_else(|| monitors.first())
}

fn monitor_containing_point(
    monitors: &[MonitorGeometry],
    point: PhysicalPosition<i32>,
) -> Option<&MonitorGeometry> {
    monitors.iter().find(|monitor| {
        let area = monitor.work_area;
        let right = i64::from(area.x) + i64::from(area.width);
        let bottom = i64::from(area.y) + i64::from(area.height);
        i64::from(point.x) >= i64::from(area.x)
            && i64::from(point.x) < right
            && i64::from(point.y) >= i64::from(area.y)
            && i64::from(point.y) < bottom
    })
}

fn window_size_for_monitor(monitor: &MonitorGeometry) -> PhysicalSize<u32> {
    PhysicalSize::new(
        scaled_logical(WINDOW_WIDTH, monitor.scale_factor),
        scaled_logical(WINDOW_HEIGHT, monitor.scale_factor),
    )
}

fn scaled_logical(value: f64, scale_factor: f64) -> u32 {
    (value * scale_factor).round().clamp(1.0, u32::MAX as f64) as u32
}

fn edge_margin(monitor: &MonitorGeometry) -> i32 {
    (EDGE_MARGIN * monitor.scale_factor)
        .round()
        .clamp(1.0, i32::MAX as f64) as i32
}

fn default_position(monitor: &MonitorGeometry) -> PhysicalPosition<i32> {
    let size = window_size_for_monitor(monitor);
    let margin = edge_margin(monitor);
    let area = monitor.work_area;
    clamp_position(
        PhysicalPosition::new(
            saturating_i64_to_i32(
                i64::from(area.x) + i64::from(area.width)
                    - i64::from(size.width)
                    - i64::from(margin),
            ),
            saturating_i64_to_i32(
                i64::from(area.y) + i64::from(area.height)
                    - i64::from(size.height)
                    - i64::from(margin),
            ),
        ),
        size,
        area,
    )
}

fn snap_and_clamp_position(
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    monitor: &MonitorGeometry,
) -> PhysicalPosition<i32> {
    let area = monitor.work_area;
    let clamped = clamp_position(position, size, area);
    let (min_x, max_x, min_y, max_y) = position_bounds(size, area);
    let threshold = edge_margin(monitor);
    PhysicalPosition::new(
        snap_axis(clamped.x, min_x, max_x, threshold),
        snap_axis(clamped.y, min_y, max_y, threshold),
    )
}

fn snap_axis(value: i32, min: i32, max: i32, threshold: i32) -> i32 {
    if i64::from(value).abs_diff(i64::from(min)) <= threshold as u64 {
        min
    } else if i64::from(value).abs_diff(i64::from(max)) <= threshold as u64 {
        max
    } else {
        value
    }
}

fn clamp_position(
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    work_area: WorkArea,
) -> PhysicalPosition<i32> {
    let (min_x, max_x, min_y, max_y) = position_bounds(size, work_area);
    PhysicalPosition::new(
        position.x.clamp(min_x, max_x),
        position.y.clamp(min_y, max_y),
    )
}

fn position_bounds(size: PhysicalSize<u32>, work_area: WorkArea) -> (i32, i32, i32, i32) {
    let min_x = work_area.x;
    let min_y = work_area.y;
    let max_x = saturating_i64_to_i32(
        (i64::from(work_area.x) + i64::from(work_area.width) - i64::from(size.width))
            .max(i64::from(min_x)),
    );
    let max_y = saturating_i64_to_i32(
        (i64::from(work_area.y) + i64::from(work_area.height) - i64::from(size.height))
            .max(i64::from(min_y)),
    );
    (min_x, max_x, min_y, max_y)
}

fn intersection_area(
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    area: WorkArea,
) -> u64 {
    let left = i64::from(position.x).max(i64::from(area.x));
    let top = i64::from(position.y).max(i64::from(area.y));
    let right = (i64::from(position.x) + i64::from(size.width))
        .min(i64::from(area.x) + i64::from(area.width));
    let bottom = (i64::from(position.y) + i64::from(size.height))
        .min(i64::from(area.y) + i64::from(area.height));
    if right <= left || bottom <= top {
        return 0;
    }
    ((right - left) as u64).saturating_mul((bottom - top) as u64)
}

fn saturating_i64_to_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn monitor(id: &str, x: i32, y: i32, width: u32, height: u32, scale: f64) -> MonitorGeometry {
        MonitorGeometry {
            id: id.to_string(),
            work_area: WorkArea {
                x,
                y,
                width,
                height,
            },
            scale_factor: scale,
        }
    }

    #[test]
    fn single_monitor_edges_snap_within_sixteen_logical_pixels() {
        let display = monitor("primary", 0, 0, 1920, 1040, 1.0);
        let size = window_size_for_monitor(&display);
        assert_eq!(
            snap_and_clamp_position(PhysicalPosition::new(1668, 797), size, &display),
            PhysicalPosition::new(1680, 808)
        );
    }

    #[test]
    fn negative_coordinate_secondary_monitor_is_preserved() {
        let displays = vec![
            monitor("primary", 0, 0, 1920, 1040, 1.0),
            monitor("left", -1920, 0, 1920, 1040, 1.0),
        ];
        let saved = MeetingCompanionPosition {
            x: -1800,
            y: 120,
            monitor_id: Some("left".to_string()),
        };
        let placement =
            restore_placement(Some(&saved), &displays, Some("primary"), Some("primary"))
                .expect("placement");
        assert_eq!(placement.position, PhysicalPosition::new(-1800, 120));
        assert_eq!(placement.monitor_id, "left");
    }

    #[test]
    fn out_of_bounds_window_is_clamped_to_work_area() {
        let display = monitor("primary", 0, 0, 1366, 728, 1.0);
        let position = clamp_position(
            PhysicalPosition::new(1300, 700),
            window_size_for_monitor(&display),
            display.work_area,
        );
        assert_eq!(position, PhysicalPosition::new(1126, 496));
    }

    #[test]
    fn dpi_and_resolution_changes_recompute_physical_window_bounds() {
        let display_125 = monitor("display", 0, 0, 1920, 1040, 1.25);
        assert_eq!(
            window_size_for_monitor(&display_125),
            PhysicalSize::new(300, 290)
        );
        let display_150 = monitor("display", 0, 0, 1600, 860, 1.5);
        let saved = MeetingCompanionPosition {
            x: 1400,
            y: 800,
            monitor_id: Some("display".to_string()),
        };
        let placement =
            restore_placement(Some(&saved), &[display_150], None, None).expect("placement");
        assert_eq!(placement.position, PhysicalPosition::new(1240, 512));
    }

    #[test]
    fn removed_monitor_falls_back_to_cursor_monitor_bottom_right() {
        let displays = vec![
            monitor("primary", 0, 0, 1920, 1040, 1.0),
            monitor("right", 1920, 0, 2560, 1400, 1.25),
        ];
        let saved = MeetingCompanionPosition {
            x: -1600,
            y: 200,
            monitor_id: Some("removed".to_string()),
        };
        let placement = restore_placement(Some(&saved), &displays, Some("right"), Some("primary"))
            .expect("placement");
        assert_eq!(placement.position, PhysicalPosition::new(4160, 1090));
        assert_eq!(placement.monitor_id, "right");
    }

    #[test]
    fn position_lock_prevents_drag_start() {
        assert!(!drag_allowed(true));
        assert!(drag_allowed(false));
    }

    #[test]
    fn companion_only_commands_reject_other_windows() {
        assert!(ensure_companion_invoker("meeting-companion").is_ok());
        assert!(ensure_companion_invoker("main").is_err());
        assert!(ensure_companion_invoker("capsule").is_err());
    }

    #[test]
    fn simultaneous_creation_claims_allow_only_one_window() {
        let state = Arc::new(Mutex::new(LifecycleState::default()));
        let wins = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let state = Arc::clone(&state);
                let wins = Arc::clone(&wins);
                std::thread::spawn(move || {
                    if state.lock().claim_window_creation(false) {
                        wins.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("creation test thread");
        }
        assert_eq!(wins.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn manual_hide_suppresses_only_the_current_meeting() {
        let mut state = LifecycleState::default();
        assert!(state.meeting_started("meeting-a", true));
        state.manual_hide();
        assert!(!state.meeting_started("meeting-a", true));
        assert!(state.meeting_started("meeting-b", true));
    }

    #[test]
    fn completed_dismissal_only_clears_the_matching_meeting() {
        let mut state = LifecycleState::default();
        assert!(state.meeting_started("meeting-a", true));
        assert!(!state.completion_finished("meeting-old"));
        assert_eq!(state.active_meeting_id.as_deref(), Some("meeting-a"));
        assert!(state.completion_finished("meeting-a"));
        assert_eq!(state.active_meeting_id, None);
    }

    #[test]
    fn failed_summary_only_dismisses_a_manually_hidden_meeting() {
        let mut state = LifecycleState::default();
        assert!(state.meeting_started("meeting-a", true));
        assert!(!state.hidden_failure_finished("meeting-a"));
        state.manual_hide();
        assert!(!state.hidden_failure_finished("meeting-old"));
        assert!(state.hidden_failure_finished("meeting-a"));
        assert_eq!(state.active_meeting_id, None);
    }

    #[test]
    fn failed_summary_dismissal_delay_is_three_seconds() {
        assert_eq!(FAILED_DISMISS_DELAY, Duration::from_secs(3));
    }
}
