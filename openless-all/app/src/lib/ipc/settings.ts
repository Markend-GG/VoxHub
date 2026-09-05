import type { AsrProviderCapabilities, StyleSystemPrompts, UserPreferences } from "../types"
export type { UpdateChannel } from "../types"
import { invokeOrMock } from "./shared"
import { mockAsrProviderCapabilities, mockSettings, mockDefaultStyleSystemPrompts, mockSetSettings } from "./mock-data"

export function getSettings(): Promise<UserPreferences> {
    return invokeOrMock("get_settings", undefined, () => ({ ...mockSettings }))
}

export function getDefaultStyleSystemPrompts(): Promise<StyleSystemPrompts> {
    return invokeOrMock("get_default_style_system_prompts", undefined, () => ({
        ...mockDefaultStyleSystemPrompts,
    }))
}

export function setSettings(prefs: UserPreferences): Promise<UserPreferences> {
    return invokeOrMock("set_settings", { prefs }, () => {
        mockSetSettings(prefs)
        return prefs
    })
}

export function listAsrProviderCapabilities(): Promise<AsrProviderCapabilities[]> {
    return invokeOrMock("list_asr_provider_capabilities", undefined, () => mockAsrProviderCapabilities)
}
