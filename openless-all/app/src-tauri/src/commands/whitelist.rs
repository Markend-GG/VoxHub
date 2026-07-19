//! 截图白名单管理 IPC 命令。

use super::*;
use crate::types::{OpenWindowApp, ScreenshotAggregationStatus};
use std::sync::Arc;

/// IPC 输入类型：添加白名单应用
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotWhitelistAppInput {
    pub display_name: String,
    pub process_name: String,
    pub exe_path: Option<String>,
}

/// 枚举当前可见窗口应用（用于"添加应用"弹窗）
#[tauri::command]
pub fn list_open_window_apps() -> Result<Vec<OpenWindowApp>, String> {
    #[cfg(target_os = "windows")]
    {
        Ok(crate::screenshot_whitelist::list_open_window_apps())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(Vec::new())
    }
}

/// 设置截图白名单开关
#[tauri::command]
pub fn set_screenshot_whitelist_enabled(
    coord: CoordinatorState<'_>,
    app: AppHandle,
    enabled: bool,
) -> Result<UserPreferences, String> {
    let mut prefs = coord.prefs().get();
    prefs.screenshot_whitelist_enabled = enabled;
    coord
        .prefs()
        .set(prefs.clone())
        .map_err(|e| e.to_string())?;
    let _ = app.emit("prefs:changed", &prefs);
    Ok(prefs)
}

/// 添加白名单应用
#[tauri::command]
pub fn add_screenshot_whitelist_app(
    coord: CoordinatorState<'_>,
    app: AppHandle,
    app_input: ScreenshotWhitelistAppInput,
) -> Result<UserPreferences, String> {
    let mut prefs = coord.prefs().get();
    let added = crate::screenshot_whitelist::add_app_to_whitelist(
        &mut prefs.screenshot_whitelist_apps,
        app_input.display_name,
        app_input.process_name,
        app_input.exe_path,
    );
    if !added {
        return Err("appAlreadyInWhitelist".to_string());
    }
    coord
        .prefs()
        .set(prefs.clone())
        .map_err(|e| e.to_string())?;
    let _ = app.emit("prefs:changed", &prefs);
    Ok(prefs)
}

/// 移除白名单应用
#[tauri::command]
pub fn remove_screenshot_whitelist_app(
    coord: CoordinatorState<'_>,
    app: AppHandle,
    process_name: String,
) -> Result<UserPreferences, String> {
    let mut prefs = coord.prefs().get();
    crate::screenshot_whitelist::remove_app_from_whitelist(
        &mut prefs.screenshot_whitelist_apps,
        &process_name,
    );
    coord
        .prefs()
        .set(prefs.clone())
        .map_err(|e| e.to_string())?;
    let _ = app.emit("prefs:changed", &prefs);
    Ok(prefs)
}

/// 恢复默认白名单（补充缺失的默认项，不删除用户自定义项）
#[tauri::command]
pub fn restore_default_screenshot_whitelist_apps(
    coord: CoordinatorState<'_>,
    app: AppHandle,
) -> Result<UserPreferences, String> {
    let mut prefs = coord.prefs().get();
    crate::screenshot_whitelist::restore_default_whitelist(&mut prefs.screenshot_whitelist_apps);
    coord
        .prefs()
        .set(prefs.clone())
        .map_err(|e| e.to_string())?;
    let _ = app.emit("prefs:changed", &prefs);
    Ok(prefs)
}

/// 设置按应用聚合分析开关
#[tauri::command]
pub fn set_screenshot_app_aggregation_enabled(
    coord: CoordinatorState<'_>,
    app: AppHandle,
    enabled: bool,
) -> Result<UserPreferences, String> {
    let mut prefs = coord.prefs().get();
    prefs.screenshot_app_aggregation_enabled = enabled;
    coord
        .prefs()
        .set(prefs.clone())
        .map_err(|e| e.to_string())?;
    let _ = app.emit("prefs:changed", &prefs);

    // 运行时启用：先 finalize 遗留桶，再启动后台定时器
    if enabled {
        let inner = Arc::clone(&coord.inner);
        std::thread::Builder::new()
            .name("openless-agg-runtime-startup".into())
            .spawn(move || {
                crate::screenshot_aggregation::finalize_expired_buckets(&inner);
            })
            .ok();
        crate::screenshot_aggregation::start_aggregation_timer(Arc::clone(&coord.inner));
    }
    Ok(prefs)
}

/// 查询当前聚合状态（待聚合桶列表）
#[tauri::command]
pub fn get_screenshot_aggregation_status(
    coord: CoordinatorState<'_>,
) -> Result<ScreenshotAggregationStatus, String> {
    let status =
        crate::screenshot_aggregation::get_aggregation_status(&coord.inner.screenshot_aggregation);
    Ok(status)
}
