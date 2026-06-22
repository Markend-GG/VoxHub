import type { ShortcutBinding } from "../types"
import { invokeOrMock } from "./shared"

export interface RewriteHistoryEntry {
  id: string
  createdAt: string
  sourceText: string
  rewrittenText: string
  stylePackId?: string | null
  stylePackName?: string | null
  appName?: string | null
  insertStatus: string
  errorCode?: string | null
  durationMs?: number | null
}

export function listRewriteHistory(): Promise<RewriteHistoryEntry[]> {
  return invokeOrMock("list_rewrite_history", undefined, () => [])
}

export function deleteRewriteHistoryEntry(id: string): Promise<void> {
  return invokeOrMock("delete_rewrite_history_entry", { id }, () => undefined)
}

export function clearRewriteHistory(): Promise<void> {
  return invokeOrMock("clear_rewrite_history", undefined, () => undefined)
}

export function setRewriteHotkey(binding: ShortcutBinding | null): Promise<void> {
  return invokeOrMock("set_rewrite_hotkey", { binding }, () => undefined)
}

export function runRewriteSelectedText(): Promise<void> {
  return invokeOrMock("run_rewrite_selected_text", undefined, () => undefined)
}
