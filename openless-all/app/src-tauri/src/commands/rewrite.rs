use super::*;
use crate::types::ContextCaptureHistoryType;

#[tauri::command]
pub fn list_rewrite_history(
    coord: CoordinatorState<'_>,
) -> Result<Vec<RewriteHistoryEntry>, String> {
    let mut entries = coord.rewrite_history().list().map_err(|e| e.to_string())?;
    match (coord.context_capture().list(), coord.context_analysis().list()) {
        (Ok(mut context_entries), Ok(analysis_entries)) => {
            crate::persistence::enrich_context_entries_with_analysis(
                &mut context_entries,
                &analysis_entries,
            );
            crate::persistence::enrich_rewrite_history_with_context(&mut entries, &context_entries);
        }
        (Ok(context_entries), Err(error)) => {
            log::warn!("[context-analysis] failed to enrich rewrite history: {error}");
            crate::persistence::enrich_rewrite_history_with_context(&mut entries, &context_entries);
        }
        (Err(error), _) => {
            log::warn!("[context-capture] failed to enrich rewrite history: {error}");
        }
    }
    Ok(entries)
}

#[tauri::command]
pub fn delete_rewrite_history_entry(coord: CoordinatorState<'_>, id: String) -> Result<(), String> {
    coord
        .rewrite_history()
        .delete(&id)
        .map_err(|e| e.to_string())?;
    if let Err(error) = coord
        .context_capture()
        .delete_for_history(ContextCaptureHistoryType::Rewrite, &id)
    {
        log::warn!("[context-capture] failed to delete rewrite context for {id}: {error}");
    }
    if let Err(error) = coord
        .context_analysis()
        .delete_for_history(ContextCaptureHistoryType::Rewrite, &id)
    {
        log::warn!("[context-analysis] failed to delete rewrite analysis for {id}: {error}");
    }
    Ok(())
}

#[tauri::command]
pub fn clear_rewrite_history(coord: CoordinatorState<'_>) -> Result<(), String> {
    coord.rewrite_history().clear().map_err(|e| e.to_string())?;
    if let Err(error) = coord
        .context_capture()
        .clear_for_history_type(ContextCaptureHistoryType::Rewrite)
    {
        log::warn!("[context-capture] failed to clear rewrite contexts: {error}");
    }
    if let Err(error) = coord
        .context_analysis()
        .clear_for_history_type(ContextCaptureHistoryType::Rewrite)
    {
        log::warn!("[context-analysis] failed to clear rewrite analysis: {error}");
    }
    Ok(())
}

/// 手动触发文本重写（测试/调试用）。正常路径通过快捷键触发。
#[tauri::command]
pub fn run_rewrite_selected_text(coord: CoordinatorState<'_>) -> Result<(), String> {
    coord.trigger_rewrite();
    Ok(())
}
