use super::*;

#[tauri::command]
pub fn list_rewrite_history(coord: CoordinatorState<'_>) -> Result<Vec<RewriteHistoryEntry>, String> {
    coord.rewrite_history().list().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_rewrite_history_entry(coord: CoordinatorState<'_>, id: String) -> Result<(), String> {
    coord.rewrite_history().delete(&id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_rewrite_history(coord: CoordinatorState<'_>) -> Result<(), String> {
    coord.rewrite_history().clear().map_err(|e| e.to_string())
}

/// 手动触发文本重写（测试/调试用）。正常路径通过快捷键触发。
#[tauri::command]
pub fn run_rewrite_selected_text(coord: CoordinatorState<'_>) -> Result<(), String> {
    coord.trigger_rewrite();
    Ok(())
}
