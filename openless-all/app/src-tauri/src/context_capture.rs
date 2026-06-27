//! Sidecar context capture for voice and rewrite history.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use uuid::Uuid;

use crate::persistence::ContextCaptureStore;
use crate::types::{
    ContextCaptureEntry, ContextCaptureHistoryType, ContextCaptureSource, ContextCaptureStatus,
};

const ERROR_UNSUPPORTED: &str = "unsupportedPlatform";
const ERROR_TITLE_FAILED: &str = "windowTitleFailed";
const ERROR_ACTIVE_WINDOW_SCREENSHOT_FAILED: &str = "activeWindowScreenshotFailed";
const ERROR_FULLSCREEN_SCREENSHOT_FAILED: &str = "fullScreenScreenshotFailed";
const ERROR_TIMEOUT: &str = "contextCaptureTimeout";
const ERROR_WORKER_DISCONNECTED: &str = "contextCaptureWorkerDisconnected";
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);
const RECENT_PRIMARY_CAPTURE_LIMIT: usize = 16;

#[derive(Debug, Clone)]
pub(crate) struct RecentPrimaryCapture {
    pub id: String,
    pub captured_at: Instant,
    pub history_type: ContextCaptureHistoryType,
    pub window_title: Option<String>,
    pub context_app: Option<String>,
    pub conversation_window: Option<String>,
}

static RECENT_PRIMARY_CAPTURES: OnceLock<Mutex<VecDeque<RecentPrimaryCapture>>> = OnceLock::new();

fn recent_primary_captures() -> &'static Mutex<VecDeque<RecentPrimaryCapture>> {
    RECENT_PRIMARY_CAPTURES.get_or_init(|| Mutex::new(VecDeque::new()))
}

#[derive(Debug, Clone)]
pub(crate) struct WindowIdentity {
    pub window_title: Option<String>,
    pub context_app: Option<String>,
    pub conversation_window: Option<String>,
}

pub(crate) fn current_window_identity() -> Option<WindowIdentity> {
    #[cfg(target_os = "windows")]
    {
        let title = windows_capture::foreground_window_title().ok()?;
        if title.trim().is_empty() {
            return None;
        }
        let (context_app, conversation_window) = parse_window_title(&title);
        Some(WindowIdentity {
            window_title: Some(title),
            context_app,
            conversation_window,
        })
    }

    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

pub(crate) fn remember_recent_primary_capture(
    history_type: ContextCaptureHistoryType,
    id: String,
) {
    if !matches!(
        history_type,
        ContextCaptureHistoryType::Voice | ContextCaptureHistoryType::Rewrite
    ) {
        return;
    }
    let identity = current_window_identity();
    let recent = RecentPrimaryCapture {
        id,
        captured_at: Instant::now(),
        history_type,
        window_title: identity.as_ref().and_then(|value| value.window_title.clone()),
        context_app: identity.as_ref().and_then(|value| value.context_app.clone()),
        conversation_window: identity.and_then(|value| value.conversation_window),
    };
    let mut captures = recent_primary_captures().lock();
    captures.push_back(recent);
    while captures.len() > RECENT_PRIMARY_CAPTURE_LIMIT {
        captures.pop_front();
    }
}

pub(crate) fn take_recent_primary_capture_matching<F>(
    window: Duration,
    mut matches_identity: F,
) -> Option<RecentPrimaryCapture>
where
    F: FnMut(&RecentPrimaryCapture) -> bool,
{
    let mut captures = recent_primary_captures().lock();
    captures.retain(|capture| capture.captured_at.elapsed() <= window);
    let index = captures
        .iter()
        .position(|capture| matches_identity(capture))?;
    captures.remove(index)
}

pub fn capture_and_store(
    store: &ContextCaptureStore,
    history_type: ContextCaptureHistoryType,
    history_id: String,
    retention_days: u32,
    max_entries: Option<u32>,
) -> Result<()> {
    let id = Uuid::new_v4().to_string();
    capture_and_store_with_id(store, id, history_type, history_id, retention_days, max_entries)
}

pub fn capture_and_store_with_id(
    store: &ContextCaptureStore,
    id: String,
    history_type: ContextCaptureHistoryType,
    history_id: String,
    retention_days: u32,
    max_entries: Option<u32>,
) -> Result<()> {
    let capture = capture_context_with_timeout(store, id, history_type, history_id);
    store.append_with_retention(capture, retention_days, max_entries)
}

fn capture_context_with_timeout(
    store: &ContextCaptureStore,
    id: String,
    history_type: ContextCaptureHistoryType,
    history_id: String,
) -> ContextCaptureEntry {
    let screenshot_path = store.screenshot_path_for_id(&id);
    let (tx, rx) = std::sync::mpsc::channel();
    let worker_id = id.clone();
    let worker_history_id = history_id.clone();
    let worker_screenshot_path = screenshot_path.clone();
    let cleanup_screenshot_path = worker_screenshot_path.clone();

    let spawn_result = std::thread::Builder::new()
        .name("openless-context-capture-worker".into())
        .spawn(move || {
            let capture = capture_context(
                worker_id,
                worker_screenshot_path,
                history_type,
                worker_history_id,
            );
            if tx.send(capture).is_err() {
                let _ = std::fs::remove_file(cleanup_screenshot_path);
            }
        });

    if let Err(error) = spawn_result {
        return failed_context_entry(
            id,
            history_type,
            history_id,
            Some(format!("{ERROR_WORKER_DISCONNECTED}: {error}")),
        );
    }

    match rx.recv_timeout(CAPTURE_TIMEOUT) {
        Ok(capture) => capture,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => failed_context_entry(
            id,
            history_type,
            history_id,
            Some(ERROR_TIMEOUT.to_string()),
        ),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => failed_context_entry(
            id,
            history_type,
            history_id,
            Some(ERROR_WORKER_DISCONNECTED.to_string()),
        ),
    }
}

fn failed_context_entry(
    id: String,
    history_type: ContextCaptureHistoryType,
    history_id: String,
    error_code: Option<String>,
) -> ContextCaptureEntry {
    ContextCaptureEntry {
        id,
        created_at: chrono::Utc::now().to_rfc3339(),
        context_app: None,
        conversation_window: None,
        window_title: None,
        capture_status: ContextCaptureStatus::Failed,
        capture_source: None,
        screenshot_path: None,
        screenshot_ref: None,
        linked_history_type: history_type,
        linked_history_id: history_id,
        error_code,
        analysis: None,
    }
}

fn capture_context(
    id: String,
    screenshot_path: std::path::PathBuf,
    history_type: ContextCaptureHistoryType,
    history_id: String,
) -> ContextCaptureEntry {
    #[cfg(target_os = "windows")]
    {
        let title_result = windows_capture::foreground_window_title();
        let title = title_result.ok();
        let parsed = title.as_deref().map(parse_window_title);
        let (context_app, conversation_window) = parsed.unwrap_or((None, None));
        let mut error_code = if title.is_some() {
            None
        } else {
            Some(ERROR_TITLE_FAILED.to_string())
        };

        match windows_capture::capture_active_window_to_bmp(&screenshot_path) {
            Ok(()) => ContextCaptureEntry {
                id: id.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
                context_app,
                conversation_window,
                window_title: title,
                capture_status: ContextCaptureStatus::Success,
                capture_source: Some(ContextCaptureSource::ActiveWindow),
                screenshot_path: Some(screenshot_path.to_string_lossy().into_owned()),
                screenshot_ref: Some(format!("{id}.bmp")),
                linked_history_type: history_type,
                linked_history_id: history_id,
                error_code,
                analysis: None,
            },
            Err(active_error) => {
                log::warn!("[context-capture] active window screenshot failed: {active_error}");
                match windows_capture::capture_full_screen_to_bmp(&screenshot_path) {
                    Ok(()) => {
                        error_code = Some(ERROR_ACTIVE_WINDOW_SCREENSHOT_FAILED.to_string());
                        ContextCaptureEntry {
                            id: id.clone(),
                            created_at: chrono::Utc::now().to_rfc3339(),
                            context_app,
                            conversation_window,
                            window_title: title,
                            capture_status:
                                ContextCaptureStatus::ActiveWindowFailedFullScreenSuccess,
                            capture_source: Some(ContextCaptureSource::FullScreen),
                            screenshot_path: Some(screenshot_path.to_string_lossy().into_owned()),
                            screenshot_ref: Some(format!("{id}.bmp")),
                            linked_history_type: history_type,
                            linked_history_id: history_id,
                            error_code,
                            analysis: None,
                        }
                    }
                    Err(full_error) => {
                        log::warn!("[context-capture] full screen screenshot failed: {full_error}");
                        let error_code = Some(format!(
                            "{ERROR_FULLSCREEN_SCREENSHOT_FAILED}: {full_error}"
                        ));
                        ContextCaptureEntry {
                            id,
                            created_at: chrono::Utc::now().to_rfc3339(),
                            context_app,
                            conversation_window,
                            window_title: title,
                            capture_status: ContextCaptureStatus::Failed,
                            capture_source: None,
                            screenshot_path: None,
                            screenshot_ref: None,
                            linked_history_type: history_type,
                            linked_history_id: history_id,
                            error_code,
                            analysis: None,
                        }
                    }
                }
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = screenshot_path;
        ContextCaptureEntry {
            id,
            created_at: chrono::Utc::now().to_rfc3339(),
            context_app: None,
            conversation_window: None,
            window_title: None,
            capture_status: ContextCaptureStatus::Unsupported,
            capture_source: None,
            screenshot_path: None,
            screenshot_ref: None,
            linked_history_type: history_type,
            linked_history_id: history_id,
            error_code: Some(ERROR_UNSUPPORTED.to_string()),
            analysis: None,
        }
    }
}

fn parse_window_title(title: &str) -> (Option<String>, Option<String>) {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return (None, None);
    }
    for separator in [" - ", " — ", " – ", " | "] {
        if let Some((left, right)) = trimmed.rsplit_once(separator) {
            let app = non_empty(right);
            let window = non_empty(left);
            return (app, window);
        }
    }
    (Some(trimmed.to_string()), Some(trimmed.to_string()))
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(target_os = "windows")]
mod windows_capture {
    use super::*;
    use std::ffi::c_void;
    use std::mem::size_of;

    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS,
        HBITMAP, HDC, SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetSystemMetrics, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
        SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };

    pub fn foreground_window_title() -> Result<String> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                anyhow::bail!("foreground window unavailable");
            }
            let len = GetWindowTextLengthW(hwnd);
            if len <= 0 {
                anyhow::bail!("window title empty");
            }
            let mut buf = vec![0u16; (len + 1) as usize];
            let copied = GetWindowTextW(hwnd, &mut buf);
            if copied <= 0 {
                anyhow::bail!("read window title failed");
            }
            let title = String::from_utf16_lossy(&buf[..copied as usize]);
            if title.trim().is_empty() {
                anyhow::bail!("window title empty");
            }
            Ok(title)
        }
    }

    pub fn capture_active_window_to_bmp(path: &Path) -> Result<()> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                anyhow::bail!("foreground window unavailable");
            }
            let mut rect = RECT::default();
            GetWindowRect(hwnd, &mut rect).context("GetWindowRect failed")?;
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;
            if width <= 0 || height <= 0 {
                anyhow::bail!("invalid window rect");
            }
            capture_screen_rect_to_bmp(rect.left, rect.top, width, height, path, true)
        }
    }

    pub fn capture_full_screen_to_bmp(path: &Path) -> Result<()> {
        unsafe {
            let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            if width <= 0 || height <= 0 {
                anyhow::bail!("invalid virtual screen");
            }
            capture_screen_rect_to_bmp(x, y, width, height, path, false)
        }
    }

    unsafe fn capture_screen_rect_to_bmp(
        source_x: i32,
        source_y: i32,
        width: i32,
        height: i32,
        path: &Path,
        reject_near_black: bool,
    ) -> Result<()> {
        let source_dc = GetDC(HWND::default());
        if source_dc.0.is_null() {
            anyhow::bail!("source dc unavailable");
        }
        let result = capture_from_dc(
            source_dc,
            source_x,
            source_y,
            width,
            height,
            path,
            reject_near_black,
        );
        let _ = ReleaseDC(HWND::default(), source_dc);
        result
    }

    unsafe fn capture_from_dc(
        source_dc: HDC,
        source_x: i32,
        source_y: i32,
        width: i32,
        height: i32,
        path: &Path,
        reject_near_black: bool,
    ) -> Result<()> {
        let mem_dc = CreateCompatibleDC(source_dc);
        if mem_dc.0.is_null() {
            anyhow::bail!("memory dc unavailable");
        }
        let bitmap = CreateCompatibleBitmap(source_dc, width, height);
        if bitmap.0.is_null() {
            let _ = DeleteDC(mem_dc);
            anyhow::bail!("bitmap unavailable");
        }
        let old_object = SelectObject(mem_dc, bitmap);
        let blt_result = BitBlt(
            mem_dc,
            0,
            0,
            width,
            height,
            source_dc,
            source_x,
            source_y,
            SRCCOPY | CAPTUREBLT,
        );
        let bytes_result = if blt_result.is_ok() {
            bitmap_bytes(mem_dc, bitmap, width, height, reject_near_black)
        } else {
            Err(anyhow::anyhow!("BitBlt failed"))
        };
        let _ = SelectObject(mem_dc, old_object);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(mem_dc);
        let bytes = bytes_result?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir failed: {}", parent.display()))?;
        }
        std::fs::write(path, bytes)
            .with_context(|| format!("write screenshot failed: {}", path.display()))
    }

    unsafe fn bitmap_bytes(
        dc: HDC,
        bitmap: HBITMAP,
        width: i32,
        height: i32,
        reject_near_black: bool,
    ) -> Result<Vec<u8>> {
        let width_u32 = width as u32;
        let height_u32 = height as u32;
        let pixel_bytes = width_u32
            .checked_mul(height_u32)
            .and_then(|px| px.checked_mul(4))
            .ok_or_else(|| anyhow::anyhow!("bitmap too large"))?;
        let mut info = BITMAPINFO::default();
        info.bmiHeader.biSize = size_of::<windows::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = width;
        info.bmiHeader.biHeight = -height;
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        info.bmiHeader.biSizeImage = pixel_bytes;

        let mut pixels = vec![0u8; pixel_bytes as usize];
        let scanlines = GetDIBits(
            dc,
            bitmap,
            0,
            height_u32,
            Some(pixels.as_mut_ptr() as *mut c_void),
            &mut info,
            DIB_RGB_COLORS,
        );
        if scanlines == 0 {
            anyhow::bail!("GetDIBits failed");
        }
        if reject_near_black && is_near_black_frame(&pixels) {
            anyhow::bail!("captured near-black frame");
        }
        Ok(build_bmp(width_u32, height_u32, pixels))
    }

    fn is_near_black_frame(pixels: &[u8]) -> bool {
        let total = pixels.len() / 4;
        if total == 0 {
            return false;
        }
        let dark = pixels
            .chunks_exact(4)
            .filter(|px| px[0] <= 3 && px[1] <= 3 && px[2] <= 3)
            .count();
        dark.saturating_mul(100) >= total.saturating_mul(99)
    }

    fn build_bmp(width: u32, height: u32, pixels: Vec<u8>) -> Vec<u8> {
        const FILE_HEADER_SIZE: u32 = 14;
        const INFO_HEADER_SIZE: u32 = 40;
        let row_stride = ((width * 3 + 3) / 4) * 4;
        let image_size = row_stride * height;
        let pixel_offset = FILE_HEADER_SIZE + INFO_HEADER_SIZE;
        let file_size = pixel_offset + image_size;
        let mut out = Vec::with_capacity(file_size as usize);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&file_size.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&pixel_offset.to_le_bytes());
        out.extend_from_slice(&INFO_HEADER_SIZE.to_le_bytes());
        out.extend_from_slice(&(width as i32).to_le_bytes());
        out.extend_from_slice(&(-(height as i32)).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&image_size.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        let padding = (row_stride - width * 3) as usize;
        for row in 0..height as usize {
            let start = row * width as usize * 4;
            let end = start + width as usize * 4;
            for px in pixels[start..end].chunks_exact(4) {
                out.extend_from_slice(&px[..3]);
            }
            out.extend(std::iter::repeat(0).take(padding));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::parse_window_title;

    #[test]
    fn parse_title_with_app_suffix() {
        let (app, window) = parse_window_title("Project - Visual Studio Code");
        assert_eq!(app.as_deref(), Some("Visual Studio Code"));
        assert_eq!(window.as_deref(), Some("Project"));
    }

    #[test]
    fn parse_title_without_separator_uses_title_for_both_fields() {
        let (app, window) = parse_window_title("WeChat");
        assert_eq!(app.as_deref(), Some("WeChat"));
        assert_eq!(window.as_deref(), Some("WeChat"));
    }
}
