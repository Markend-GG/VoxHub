//! 文本重写编排：选区读取 -> LLM 重写 -> 插入/替换 -> 历史写入 -> 状态事件。
//!
//! 与 `polish_flow.rs` 平行，共享 active LLM provider 构建逻辑，但 prompt 独立、
//! 不涉及 ASR 语义。历史写入独立的 `rewrite-history.json`，不污染 `history.json`。
//! 状态反馈复用 capsule 窗口（与录音共享），不另建浮窗。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::selection::capture_selection;
use crate::types::{
    rewrite_error_code, InsertStatus, RewriteHistoryEntry, RewriteStatePayload, RewriteStateKind,
};

use super::*;

/// 默认重写风格 prompt。Phase 1 不改 StylePack 数据结构，使用内置默认。
const DEFAULT_REWRITE_STYLE_PROMPT: &str =
    "改善表达的流畅度和清晰度，修正语法和标点错误，保持原文语气和正式程度。";

/// 重写编排入口。从 action hotkey bridge 线程调用，内部用 block_on 跑 async 逻辑。
pub(super) fn run_rewrite_flow(inner: &Arc<Inner>) {
    tauri::async_runtime::block_on(async move {
        run_rewrite_flow_async(inner).await;
    });
}

async fn run_rewrite_flow_async(inner: &Arc<Inner>) {
    // 1. 互斥检查：用 AtomicBool 防止并发重写
    if inner
        .rewrite_in_progress
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        log::info!("[rewrite] already in progress, ignoring hotkey");
        return;
    }

    // 确保退出时复位标志（无论成功失败）
    let result = run_rewrite_flow_impl(inner).await;
    inner.rewrite_in_progress.store(false, Ordering::SeqCst);
    result
}

async fn run_rewrite_flow_impl(inner: &Arc<Inner>) {
    // 2. 检查 dictation / QA 是否正在进行
    let dictation_phase = inner.state.lock().phase;
    if !matches!(dictation_phase, SessionPhase::Idle) {
        emit_rewrite_capsule(inner, CapsuleState::Error, Some("正在处理语音，稍后再试"));
        emit_rewrite_state(
            inner,
            RewriteStateKind::Error,
            Some("正在处理语音，稍后再试".into()),
            None,
            None,
            None,
            Some(rewrite_error_code::DICTATION_BUSY.to_string()),
        );
        schedule_capsule_idle(inner, 2000);
        return;
    }
    
    let qa_phase = inner.qa_state.lock().phase;
    if !matches!(qa_phase, QaPhase::Idle) {
        emit_rewrite_capsule(inner, CapsuleState::Error, Some("当前问答进行中，稍后再试"));
        emit_rewrite_state(
            inner,
            RewriteStateKind::Error,
            Some("当前问答进行中，稍后再试".into()),
            None,
            None,
            None,
            Some(rewrite_error_code::DICTATION_BUSY.to_string()),
        );
        schedule_capsule_idle(inner, 2000);
        return;
    }

    // 3. 记录焦点（在发送任何模拟按键之前）
    let focus_target = capture_focus_target();

    // 4. 等待修饰键释放 — 全局快捷键 Ctrl+Shift+R 触发后，用户可能还按着
    //    Ctrl+Shift，如果立刻发 Ctrl+C 模拟复制，Shift 仍按下会导致变成
    //    Ctrl+Shift+C 而非 Ctrl+C，结果 "C" 被当作普通字符输入而非复制命令。
    //    轮询等待 Shift/Ctrl/Alt 修饰键全部释放，最多等 500ms。
    emit_rewrite_capsule(inner, CapsuleState::Polishing, Some("正在读取选中文本…"));
    wait_for_modifiers_released(500);

    // 5. 读取选区（带一次重试）
    let selection = match capture_selection() {
        Some(s) => s,
        None => {
            // 首次失败可能是时序问题，等 100ms 再试一次
            std::thread::sleep(std::time::Duration::from_millis(100));
            match capture_selection() {
                Some(s) => s,
                None => {
            append_rewrite_history(
                inner,
                RewriteHistoryEntry {
                    id: new_uuid(),
                    created_at: now_rfc3339(),
                    source_text: String::new(),
                    rewritten_text: String::new(),
                    style_pack_id: None,
                    style_pack_name: None,
                    app_name: None,
                    insert_status: InsertStatus::Failed,
                    error_code: Some(rewrite_error_code::SELECTION_EMPTY.to_string()),
                    duration_ms: None,
                },
            );
            emit_rewrite_capsule(inner, CapsuleState::Error, Some("先选中一段文字再按重写快捷键"));
            emit_rewrite_state(
                inner,
                RewriteStateKind::Error,
                Some("先选中一段文字，再按重写快捷键".into()),
                None,
                None,
                None,
                Some(rewrite_error_code::SELECTION_EMPTY.to_string()),
            );
                    schedule_capsule_idle(inner, 2000);
            return;
                }
            }
        }
    };

    // 6. emit rewriting
    let source_preview = preview_text(&selection.text);
    emit_rewrite_capsule(inner, CapsuleState::Polishing, Some("正在重写…"));
    emit_rewrite_state(
        inner,
        RewriteStateKind::Rewriting,
        None,
        Some(source_preview.clone()),
        None,
        None,
        None,
    );

    let prefs = inner.prefs.get();
    let started = std::time::Instant::now();

    // 7. LLM 调用
    let rewrite_result = rewrite_text(
        &selection.text,
        DEFAULT_REWRITE_STYLE_PROMPT,
        prefs.llm_thinking_enabled,
    )
    .await;

    let rewritten_text = match rewrite_result {
        Ok(text) => text,
        Err(err) => {
            let reason = err.to_string();
            log::error!("[rewrite] LLM failed: {reason}");
            append_rewrite_history(
                inner,
                RewriteHistoryEntry {
                    id: new_uuid(),
                    created_at: now_rfc3339(),
                    source_text: selection.text.clone(),
                    rewritten_text: String::new(),
                    style_pack_id: None,
                    style_pack_name: None,
                    app_name: selection.source_app.clone(),
                    insert_status: InsertStatus::Failed,
                    error_code: Some(rewrite_error_code::LLM_FAILED.to_string()),
                    duration_ms: Some(started.elapsed().as_millis() as u64),
                },
            );
            emit_rewrite_capsule(inner, CapsuleState::Error, Some("重写失败，请稍后重试"));
            emit_rewrite_state(
                inner,
                RewriteStateKind::Error,
                Some("重写失败，请稍后重试".into()),
                Some(source_preview),
                None,
                None,
                Some(rewrite_error_code::LLM_FAILED.to_string()),
            );
            schedule_capsule_idle(inner, 2000);
            return;
        }
    };

    // 8. 恢复焦点
    let focus_ok = restore_focus_target_if_possible(focus_target);

    // 9. emit inserting
    let result_preview = preview_text(&rewritten_text);
    emit_rewrite_capsule(inner, CapsuleState::Polishing, Some("正在替换选中文本…"));
    emit_rewrite_state(
        inner,
        RewriteStateKind::Inserting,
        None,
        Some(source_preview.clone()),
        Some(result_preview.clone()),
        None,
        None,
    );

    // 10. 插入/替换
    let (insert_status, error_code) = if focus_ok {
        let status = inner.inserter.insert(
            &rewritten_text,
            prefs.restore_clipboard_after_paste,
            prefs.paste_shortcut,
        );
        let code = match status {
            InsertStatus::Failed => Some(rewrite_error_code::INSERT_FAILED.to_string()),
            InsertStatus::CopiedFallback => {
                Some(rewrite_error_code::INSERT_FAILED.to_string())
            }
            _ => None,
        };
        (status, code)
    } else {
        // 焦点恢复失败：只复制到剪贴板，不模拟粘贴
        log::warn!("[rewrite] focus restore failed, copying to clipboard only");
        let status = inner.inserter.copy_fallback(&rewritten_text);
        (status, Some(rewrite_error_code::FOCUS_RESTORE_FAILED.to_string()))
    };

    // 11. 写历史
    append_rewrite_history(
        inner,
        RewriteHistoryEntry {
            id: new_uuid(),
            created_at: now_rfc3339(),
            source_text: selection.text.clone(),
            rewritten_text: rewritten_text.clone(),
            style_pack_id: None,
            style_pack_name: Some("默认".into()),
            app_name: selection.source_app.clone(),
            insert_status,
            error_code: error_code.clone(),
            duration_ms: Some(started.elapsed().as_millis() as u64),
        },
    );

    // 12. emit done / error
    let message = match insert_status {
        InsertStatus::Inserted | InsertStatus::PasteSent => Some("已替换"),
        InsertStatus::CopiedFallback => Some("已复制，可手动粘贴"),
        InsertStatus::Failed => Some("未能重写"),
    };
    let kind = if insert_status == InsertStatus::Failed {
        RewriteStateKind::Error
    } else {
        RewriteStateKind::Done
    };

    let capsule_state = if kind == RewriteStateKind::Error {
        CapsuleState::Error
    } else {
        CapsuleState::Done
    };
    emit_rewrite_capsule(inner, capsule_state, message);
    schedule_capsule_idle(inner, 2000);

    emit_rewrite_state(
        inner,
        kind,
        message.map(|s| s.into()),
        Some(source_preview),
        Some(result_preview),
        Some(insert_status),
        error_code,
    );
}

/// 调用共享 LLM provider 进行文本重写。镜像 `polish_flow.rs` 的 Gemini 分支模式。
async fn rewrite_text(
    source_text: &str,
    style_prompt: &str,
    llm_thinking_enabled: bool,
) -> anyhow::Result<String> {
    let active_llm = CredentialsVault::get_active_llm();
    if active_llm == "gemini" {
        let (api_key, model, base_url) = read_gemini_credentials()?;
        let provider = GeminiProvider::new(
            GeminiConfig::new(api_key, model, base_url)
                .with_thinking_enabled(llm_thinking_enabled),
        );
        return Ok(provider.rewrite(source_text, style_prompt).await?);
    }

    let provider = build_active_llm_provider(llm_thinking_enabled)?;
    Ok(provider.rewrite(source_text, style_prompt).await?)
}

fn append_rewrite_history(inner: &Arc<Inner>, entry: RewriteHistoryEntry) {
    if !inner.prefs.get().rewrite_save_history {
        return;
    }
    if let Err(e) = inner.rewrite_history.append(entry) {
        log::warn!("[rewrite] failed to write rewrite history: {e}");
    }
}

/// 通过 capsule 窗口展示重写状态（与录音共享同一个胶囊窗口）。
fn emit_rewrite_capsule(inner: &Arc<Inner>, state: CapsuleState, message: Option<&str>) {
    emit_capsule(
        inner,
        state,
        0.0,
        0,
        message.map(|s| s.to_string()),
        None,
    );
}

/// 通过 rewrite:state 事件通知前端（用于历史页面刷新等）。
fn emit_rewrite_state(
    inner: &Arc<Inner>,
    kind: RewriteStateKind,
    message: Option<String>,
    source_preview: Option<String>,
    result_preview: Option<String>,
    insert_status: Option<InsertStatus>,
    error_code: Option<String>,
) {
    let payload = RewriteStatePayload {
        kind,
        message,
        source_preview,
        result_preview,
        insert_status,
        error_code,
    };
    if let Some(app) = inner.app.lock().clone() {
        let _ = app.emit("rewrite:state", &payload);
    }
}

fn preview_text(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= 80 {
        text.to_string()
    } else {
        let head: String = chars.iter().take(77).collect();
        format!("{head}…")
    }
}

fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// 轮询等待所有修饰键（Shift/Ctrl/Alt）释放。
/// 全局快捷键触发后用户可能还按着修饰键，此时发 Ctrl+C 会变成 Ctrl+Shift+C。
/// 每 20ms 轮询一次，最多等 timeout_ms 毫秒。
#[cfg(target_os = "windows")]
fn wait_for_modifiers_released(timeout_ms: u64) {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        let shift = unsafe { GetAsyncKeyState(0x10) };
        let ctrl = unsafe { GetAsyncKeyState(0x11) };
        let alt = unsafe { GetAsyncKeyState(0x12) };
        // 高位为 1 表示当前按下
        if shift >= 0 && ctrl >= 0 && alt >= 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(not(target_os = "windows"))]
fn wait_for_modifiers_released(timeout_ms: u64) {
    // macOS / Linux 不需要此修复（macOS 走 AX 直读选区，不模拟 Ctrl+C）
    let _ = timeout_ms;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_text_short_passes_through() {
        assert_eq!(preview_text("hello"), "hello");
    }

    #[test]
    fn preview_text_long_truncates() {
        let long = "a".repeat(100);
        let preview = preview_text(&long);
        assert!(preview.ends_with('…'));
        assert!(preview.chars().count() <= 80);
    }
}
