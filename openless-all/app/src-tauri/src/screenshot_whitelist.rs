//! 截图白名单：Windows 前台进程识别、可见窗口枚举、白名单判断。

use crate::types::{ScreenshotWhitelistApp, UserPreferences};

/// 白名单判断：截图触发前调用。
///
/// 规则：
/// 1. 白名单关闭 → 允许截图。
/// 2. 白名单开启 → 读取前台进程名，小写匹配白名单列表。
/// 3. 进程识别失败 → 按未命中处理，不允许截图。
pub(crate) fn screenshot_allowed_by_whitelist(prefs: &UserPreferences) -> bool {
    if !prefs.screenshot_whitelist_enabled {
        return true;
    }
    #[cfg(target_os = "windows")]
    {
        let Some(identity) = current_foreground_app_identity() else {
            log::debug!("[whitelist] foreground app identity unavailable, denying");
            return false;
        };
        let process_name_lower = identity.process_name.to_lowercase();
        // 过滤 OpenLess 自身进程
        if is_openless_process(&process_name_lower) {
            log::debug!("[whitelist] foreground is OpenLess itself, denying");
            return false;
        }
        let allowed = prefs
            .screenshot_whitelist_apps
            .iter()
            .any(|app| app.process_name.to_lowercase() == process_name_lower);
        if !allowed {
            log::debug!(
                "[whitelist] process '{}' not in whitelist, denying",
                identity.process_name
            );
        }
        allowed
    }
    #[cfg(not(target_os = "windows"))]
    {
        // 非 Windows 平台：白名单开启时无法获取进程，按未命中处理
        if prefs.screenshot_whitelist_enabled {
            log::debug!("[whitelist] non-Windows platform with whitelist enabled, denying");
            return false;
        }
        true
    }
}

/// 判断是否为 OpenLess 自身进程
pub(crate) fn is_openless_process(process_name_lower: &str) -> bool {
    let openless_names = ["openless.exe", "openless"];
    openless_names.contains(&process_name_lower)
}

// ─── Windows 前台进程识别 ───────────────────────────────────────────────

#[cfg(target_os = "windows")]
pub(crate) fn current_foreground_app_identity() -> Option<crate::types::ForegroundAppIdentity> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    unsafe {
        let hwnd: HWND = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }

        let process_name = get_process_name_by_pid(pid)?;
        let exe_path = get_process_exe_path(pid);
        let display_name = get_file_description(&exe_path);
        let window_title = get_window_title(hwnd);

        Some(crate::types::ForegroundAppIdentity {
            process_name,
            process_id: pid,
            exe_path,
            display_name,
            window_title,
        })
    }
}

#[cfg(target_os = "windows")]
unsafe fn get_process_name_by_pid(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
    };

    let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
    let mut entry = PROCESSENTRY32 {
        dwSize: std::mem::size_of::<PROCESSENTRY32>() as u32,
        ..Default::default()
    };
    let mut result = None;
    if Process32First(snapshot, &mut entry).is_ok() {
        loop {
            if entry.th32ProcessID == pid {
                // szExeFile is [i8; 260] in windows crate - convert bytes to string
                let bytes: Vec<u8> = entry.szExeFile[..entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len())]
                    .iter()
                    .map(|&c| c as u8)
                    .collect();
                let name = String::from_utf8_lossy(&bytes).to_string();
                result = Some(name);
                break;
            }
            if Process32Next(snapshot, &mut entry).is_err() {
                break;
            }
        }
    }
    let _ = CloseHandle(snapshot);
    result
}

#[cfg(target_os = "windows")]
unsafe fn get_process_exe_path(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::{CloseHandle, MAX_PATH};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buf = vec![0u16; MAX_PATH as usize];
    let mut size = buf.len() as u32;
    let result = if QueryFullProcessImageNameW(
        handle,
        Default::default(),
        windows::core::PWSTR(buf.as_mut_ptr()),
        &mut size,
    )
    .is_ok()
    {
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    } else {
        None
    };
    let _ = CloseHandle(handle);
    result
}

#[cfg(target_os = "windows")]
unsafe fn get_file_description(_exe_path: &Option<String>) -> Option<String> {
    // V1: 文件描述获取较复杂，暂不实现，返回 None
    // displayName 回退到 processName
    None
}

#[cfg(target_os = "windows")]
unsafe fn get_window_title(hwnd: windows::Win32::Foundation::HWND) -> Option<String> {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextLengthW;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowTextW;

    let len = GetWindowTextLengthW(hwnd) as usize;
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u16; len + 1];
    let copied = GetWindowTextW(hwnd, &mut buf) as usize;
    if copied == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..copied]))
}

// ─── Windows 可见窗口应用枚举 ───────────────────────────────────────────

#[cfg(target_os = "windows")]
pub(crate) fn list_open_window_apps() -> Vec<crate::types::OpenWindowApp> {
    use std::collections::HashSet;
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible,
    };

    struct EnumData {
        apps: Vec<crate::types::OpenWindowApp>,
        seen: HashSet<String>,
    }

    unsafe extern "system" fn enum_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let data = &mut *(lparam.0 as *mut EnumData);

        // 过滤不可见窗口
        if !IsWindowVisible(hwnd).as_bool() {
            return TRUE;
        }

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return TRUE;
        }

        let Some(process_name) = get_process_name_by_pid(pid) else {
            return TRUE;
        };
        let process_name_lower = process_name.to_lowercase();

        // 过滤空标题窗口
        let window_title = get_window_title(hwnd);
        if window_title
            .as_ref()
            .map(|t| t.trim().is_empty())
            .unwrap_or(true)
        {
            return TRUE;
        }

        // 过滤 OpenLess 自身
        if is_openless_process(&process_name_lower) {
            return TRUE;
        }

        // 过滤系统进程
        let system_processes = [
            "dwm.exe",
            "explorer.exe",
            "searchui.exe",
            "searchapp.exe",
            "shellexperiencehost.exe",
            "startmenuexperiencehost.exe",
            "textinputhost.exe",
            "runtimebroker.exe",
            "systemsettings.exe",
            "applicationframehost.exe",
            "windowsinternal.composableshell.experiences.textinput.inputapp.exe",
        ];
        if system_processes.contains(&process_name_lower.as_str()) {
            return TRUE;
        }

        let exe_path = get_process_exe_path(pid);
        let dedup_key = format!("{}|{}", process_name_lower, exe_path.as_deref().unwrap_or(""));

        if data.seen.contains(&dedup_key) {
            return TRUE;
        }
        data.seen.insert(dedup_key);

        let display_name = get_file_description(&exe_path)
            .unwrap_or_else(|| process_name.trim_end_matches(".exe").to_string());

        data.apps.push(crate::types::OpenWindowApp {
            process_name,
            process_id: pid,
            display_name,
            exe_path,
            window_title,
        });

        TRUE
    }

    let mut data = EnumData {
        apps: Vec::new(),
        seen: HashSet::new(),
    };

    unsafe {
        let _ = EnumWindows(
            Some(enum_callback),
            LPARAM(&mut data as *mut EnumData as isize),
        );
    }

    data.apps
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn list_open_window_apps() -> Vec<crate::types::OpenWindowApp> {
    Vec::new()
}

// ─── 白名单管理辅助函数 ─────────────────────────────────────────────────

/// 添加应用到白名单（去重，processName 小写匹配）
pub(crate) fn add_app_to_whitelist(
    apps: &mut Vec<ScreenshotWhitelistApp>,
    display_name: String,
    process_name: String,
    exe_path: Option<String>,
) -> bool {
    let process_lower = process_name.to_lowercase();
    if apps
        .iter()
        .any(|app| app.process_name.to_lowercase() == process_lower)
    {
        return false; // 已存在
    }
    let display = if display_name.trim().is_empty() {
        process_name.clone()
    } else {
        display_name
    };
    apps.push(ScreenshotWhitelistApp {
        id: uuid::Uuid::new_v4().to_string(),
        display_name: display,
        process_name: process_lower,
        exe_path,
        source: crate::types::ScreenshotWhitelistAppSource::User,
        created_at: chrono::Utc::now().to_rfc3339(),
    });
    true
}

/// 从白名单中移除应用（按 processName 小写匹配）
pub(crate) fn remove_app_from_whitelist(
    apps: &mut Vec<ScreenshotWhitelistApp>,
    process_name: &str,
) -> bool {
    let process_lower = process_name.to_lowercase();
    let before = apps.len();
    apps.retain(|app| app.process_name.to_lowercase() != process_lower);
    apps.len() < before
}

/// 恢复默认白名单：补充缺失的默认项，不删除用户自定义项
pub(crate) fn restore_default_whitelist(apps: &mut Vec<ScreenshotWhitelistApp>) {
    let defaults = crate::types::default_screenshot_whitelist_apps_list();
    for default_app in defaults {
        let process_lower = default_app.process_name.to_lowercase();
        if !apps
            .iter()
            .any(|app| app.process_name.to_lowercase() == process_lower)
        {
            apps.push(default_app);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ScreenshotWhitelistApp, ScreenshotWhitelistAppSource, UserPreferences,
    };

    fn make_app(process_name: &str, source: ScreenshotWhitelistAppSource) -> ScreenshotWhitelistApp {
        ScreenshotWhitelistApp {
            id: uuid::Uuid::new_v4().to_string(),
            display_name: process_name.trim_end_matches(".exe").to_string(),
            process_name: process_name.to_lowercase(),
            exe_path: None,
            source,
            created_at: "2026-06-27T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn whitelist_disabled_allows_screenshot() {
        let mut prefs = UserPreferences::default();
        prefs.screenshot_whitelist_enabled = false;
        assert!(screenshot_allowed_by_whitelist(&prefs));
    }

    #[test]
    fn whitelist_enabled_empty_list_denies_all() {
        let mut prefs = UserPreferences::default();
        prefs.screenshot_whitelist_enabled = true;
        prefs.screenshot_whitelist_apps = Vec::new();
        // 非 Windows 平台上，白名单开启时无法获取进程，返回 false
        #[cfg(not(target_os = "windows"))]
        assert!(!screenshot_allowed_by_whitelist(&prefs));
    }

    #[test]
    fn add_app_deduplicates_by_process_name() {
        let mut apps = vec![make_app("chrome.exe", ScreenshotWhitelistAppSource::Default)];
        let added = add_app_to_whitelist(
            &mut apps,
            "Chrome".to_string(),
            "Chrome.exe".to_string(),
            None,
        );
        assert!(!added, "should not add duplicate");
        assert_eq!(apps.len(), 1);
    }

    #[test]
    fn add_app_inserts_new_entry() {
        let mut apps = vec![make_app("chrome.exe", ScreenshotWhitelistAppSource::Default)];
        let added = add_app_to_whitelist(
            &mut apps,
            "Firefox".to_string(),
            "firefox.exe".to_string(),
            None,
        );
        assert!(added);
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[1].process_name, "firefox.exe");
        assert_eq!(apps[1].source, ScreenshotWhitelistAppSource::User);
    }

    #[test]
    fn remove_app_deletes_matching_entry() {
        let mut apps = vec![
            make_app("chrome.exe", ScreenshotWhitelistAppSource::Default),
            make_app("msedge.exe", ScreenshotWhitelistAppSource::Default),
        ];
        let removed = remove_app_from_whitelist(&mut apps, "Chrome.exe");
        assert!(removed);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].process_name, "msedge.exe");
    }

    #[test]
    fn remove_app_returns_false_for_missing() {
        let mut apps = vec![make_app("chrome.exe", ScreenshotWhitelistAppSource::Default)];
        let removed = remove_app_from_whitelist(&mut apps, "firefox.exe");
        assert!(!removed);
        assert_eq!(apps.len(), 1);
    }

    #[test]
    fn restore_default_adds_missing_defaults() {
        let mut apps = vec![make_app("chrome.exe", ScreenshotWhitelistAppSource::Default)];
        restore_default_whitelist(&mut apps);
        // 应该补充了所有默认项
        let default_list = crate::types::default_screenshot_whitelist_apps_list();
        assert!(apps.len() >= default_list.len());
    }

    #[test]
    fn restore_default_preserves_user_entries() {
        let mut apps = vec![make_app(
            "myapp.exe",
            ScreenshotWhitelistAppSource::User,
        )];
        restore_default_whitelist(&mut apps);
        assert!(apps.iter().any(|app| app.process_name == "myapp.exe"));
    }

    #[test]
    fn restore_default_does_not_duplicate_existing() {
        let mut apps = vec![make_app("chrome.exe", ScreenshotWhitelistAppSource::Default)];
        restore_default_whitelist(&mut apps);
        let chrome_count = apps
            .iter()
            .filter(|app| app.process_name == "chrome.exe")
            .count();
        assert_eq!(chrome_count, 1);
    }

    #[test]
    fn default_whitelist_contains_expected_apps() {
        let defaults = crate::types::default_screenshot_whitelist_apps_list();
        let process_names: Vec<&str> = defaults.iter().map(|a| a.process_name.as_str()).collect();
        assert!(process_names.contains(&"chrome.exe"));
        assert!(process_names.contains(&"msedge.exe"));
        assert!(process_names.contains(&"trae.exe"));
        assert!(process_names.contains(&"wxwork.exe"));
        assert!(process_names.contains(&"winword.exe"));
        assert!(process_names.contains(&"wps.exe"));
    }

    #[test]
    fn is_openless_process_filters_correctly() {
        assert!(is_openless_process("openless.exe"));
        assert!(is_openless_process("openless"));
        assert!(!is_openless_process("chrome.exe"));
    }
}
