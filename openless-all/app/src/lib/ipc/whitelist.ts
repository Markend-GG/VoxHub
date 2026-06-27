// 截图白名单管理 IPC

import type {
  OpenWindowApp,
  ScreenshotWhitelistAppInput,
  UserPreferences,
} from "../types"
import { invokeOrMock } from "./shared"

export function listOpenWindowApps(): Promise<OpenWindowApp[]> {
  return invokeOrMock("list_open_window_apps", undefined, () => [])
}

export function setScreenshotWhitelistEnabled(
  enabled: boolean,
): Promise<UserPreferences> {
  return invokeOrMock(
    "set_screenshot_whitelist_enabled",
    { enabled },
    () => ({ screenshotWhitelistEnabled: enabled } as UserPreferences),
  )
}

export function addScreenshotWhitelistApp(
  appInput: ScreenshotWhitelistAppInput,
): Promise<UserPreferences> {
  return invokeOrMock(
    "add_screenshot_whitelist_app",
    { appInput },
    () => ({} as UserPreferences),
  )
}

export function removeScreenshotWhitelistApp(
  processName: string,
): Promise<UserPreferences> {
  return invokeOrMock(
    "remove_screenshot_whitelist_app",
    { processName },
    () => ({} as UserPreferences),
  )
}

export function restoreDefaultScreenshotWhitelistApps(): Promise<UserPreferences> {
  return invokeOrMock(
    "restore_default_screenshot_whitelist_apps",
    undefined,
    () => ({} as UserPreferences),
  )
}
