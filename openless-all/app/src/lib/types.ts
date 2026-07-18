// TypeScript mirror of src-tauri/src/types.rs.
// All keys are camelCase (Rust serializes with #[serde(rename_all = "camelCase")]).
// PolishMode is an exception — Rust uses lowercase serialization.

import type {
  AndroidAccessibilityStatus,
  AndroidInsertStrategy,
  AndroidOverlayActivationMode,
  AndroidOverlayCancelSwipeDirection,
  AndroidOverlayLeftSwipeAction,
  AndroidOverlayStatus,
  AndroidOverlayTrigger,
} from '../../android/frontend/lib/androidTypes';

export type {
  AndroidAccessibilityStatus,
  AndroidInsertStrategy,
  AndroidOverlayActivationMode,
  AndroidOverlayCancelSwipeDirection,
  AndroidOverlayLeftSwipeAction,
  AndroidOverlayStatus,
  AndroidOverlayTrigger,
};

export type PolishMode = 'raw' | 'light' | 'structured' | 'formal';

export type InsertStatus = 'inserted' | 'pasteSent' | 'copiedFallback' | 'failed';

export type ContextCaptureStatus =
  | 'success'
  | 'activeWindowFailedFullScreenSuccess'
  | 'failed'
  | 'unsupported';

export type ContextCaptureSource = 'activeWindow' | 'fullScreen';

export type ContextCaptureHistoryType = 'voice' | 'rewrite' | 'screenshotRecord';

export type ContextAnalysisStatus = 'pending' | 'success' | 'failed' | 'skipped';

// ─── 截图白名单 ──────────────────────────────────────────────────

export type ScreenshotWhitelistAppSource = 'default' | 'user';

export interface ScreenshotWhitelistApp {
  id: string;
  displayName: string;
  processName: string;
  exePath?: string | null;
  source: ScreenshotWhitelistAppSource;
  createdAt: string;
}

export interface OpenWindowApp {
  processName: string;
  processId: number;
  displayName: string;
  exePath?: string | null;
  windowTitle?: string | null;
}

export interface ScreenshotWhitelistAppInput {
  displayName: string;
  processName: string;
  exePath?: string | null;
}

export type ContextAnalysisContextType =
  | 'chat'
  | 'ai_chat'
  | 'document'
  | 'browser'
  | 'editor'
  | 'email'
  | 'meeting'
  | 'task'
  | 'settings'
  | 'unknown';

export type ContextAnalysisActivityType =
  | 'decision'
  | 'action_request'
  | 'question'
  | 'discussion'
  | 'research'
  | 'planning'
  | 'implementation'
  | 'review'
  | 'note'
  | 'unknown';

export type ContextAnalysisWorkStatus =
  | 'completed'
  | 'inProgress'
  | 'planned'
  | 'discussed'
  | 'viewed'
  | 'unknown';

export type ContextAnalysisEvidenceLevel =
  | 'explicit'
  | 'inferred'
  | 'weak'
  | 'unknown';

export interface ContextAnalysisActionItem {
  text: string;
  owner: string | null;
  dueDate: string | null;
  confidence: number;
}

export interface ContextAnalysisResult {
  id: string;
  contextCaptureId: string;
  linkedHistoryType: ContextCaptureHistoryType;
  linkedHistoryId: string;
  status: ContextAnalysisStatus;
  createdAt: string;
  analyzedAt: string | null;
  providerId: string | null;
  model: string | null;
  promptVersion: string;
  promptHash?: string | null;
  analysisGeneration?: string | null;
  schemaVersion: number;
  inputMode: string;
  imageMimeType: string | null;
  imageWidth: number | null;
  imageHeight: number | null;
  imageBytes: number | null;
  conversationName: string | null;
  briefSummary: string | null;
  fullSummary: string | null;
  detectedApp: string | null;
  detectedContextType: ContextAnalysisContextType;
  topic: string | null;
  userIntent: string | null;
  activityType: ContextAnalysisActivityType;
  workStatus: ContextAnalysisWorkStatus;
  evidenceLevel: ContextAnalysisEvidenceLevel;
  decision: string | null;
  actionItems: ContextAnalysisActionItem[];
  relatedPeople: string[];
  projectOrDomain: string | null;
  visualEvidence: string[];
  sensitiveContentVisible: boolean;
  confidence: number;
  uncertaintyReason: string | null;
  errorCode: string | null;
}

export interface ContextCaptureEntry {
  id: string;
  createdAt: string;
  contextApp: string | null;
  conversationWindow: string | null;
  windowTitle: string | null;
  captureStatus: ContextCaptureStatus;
  captureSource: ContextCaptureSource | null;
  screenshotRef: string | null;
  linkedHistoryType: ContextCaptureHistoryType;
  linkedHistoryId: string;
  errorCode: string | null;
  analysis?: ContextAnalysisResult | null;
}

export type ScreenshotRecordStatus =
  | 'collecting'
  | 'queued'
  | 'analyzing'
  | 'success'
  | 'failed'
  | 'skipped';

export interface ScreenshotRecord {
  id: string;
  createdAt: string;
  updatedAt: string;
  windowStartedAt: string;
  windowEndedAt: string | null;
  status: ScreenshotRecordStatus;
  contextApp: string | null;
  conversationWindow: string | null;
  windowTitle: string | null;
  screenshotIds: string[];
  submittedScreenshotIds: string[];
  triggerCount: number;
  errorCode: string | null;
  errorMessage: string | null;
  analysis?: ContextAnalysisResult | null;
  aggregationMode?: string | null;
  aggregationBucketId?: string | null;
  processName?: string | null;
}

export type ScreenshotAggregationBucketStatus = 'collecting' | 'finalizing' | 'failed';

export interface ScreenshotAggregationBucket {
  id: string;
  processName: string;
  appDisplayName: string | null;
  firstCapturedAt: string;
  lastCapturedAt: string;
  status: ScreenshotAggregationBucketStatus;
  screenshotIds: string[];
  triggerCount: number;
  errorCode?: string | null;
  errorMessage?: string | null;
}

export interface ScreenshotAggregationStatus {
  buckets: ScreenshotAggregationStatusBucket[];
}

export interface ScreenshotAggregationStatusBucket {
  id: string;
  processName: string;
  appDisplayName: string | null;
  screenshotCount: number;
  firstCapturedAt: string;
  lastCapturedAt: string;
}

export type ReportType = 'daily' | 'weekly' | 'monthly';

export interface ReportTemplate {
  id: string;
  reportType: ReportType;
  name: string;
  content: string;
  isBuiltin: boolean;
  createdAt: string;
  updatedAt: string;
}

export type ReportGenerationStatus = 'pending' | 'success' | 'failed' | 'skipped';

export interface GeneratedReportSourceStats {
  voiceCount: number;
  rewriteCount: number;
  screenshotRecordCount: number;
  analyzedScreenshotRecordCount: number;
}

export interface GeneratedReport {
  id: string;
  reportType: ReportType;
  title: string;
  rangeStart: string;
  rangeEnd: string;
  templateId: string;
  templateName: string;
  templateContent: string;
  userMainWork: string | null;
  status: ReportGenerationStatus;
  content: string | null;
  sourceStats: GeneratedReportSourceStats;
  errorCode: string | null;
  errorMessage: string | null;
  scheduleKey?: string | null;
  createdAt: string;
  updatedAt: string;
}

/** 概览页年度活动热力图的单日计数（date = 本地日期 YYYY-MM-DD）。 */
export interface ActivityDay {
  date: string;
  count: number;
}

export interface DictationSession {
  id: string;
  createdAt: string; // ISO-8601
  rawTranscript: string;
  finalText: string;
  mode: PolishMode;
  stylePackId: string | null;
  translationActive: boolean;
  polishSource: string | null;
  appBundleId: string | null;
  appName: string | null;
  insertStatus: InsertStatus;
  errorCode: string | null;
  durationMs: number | null;
  /** ASR 识别耗时（毫秒）：从录音结束到拿到转写结果。 */
  asrDurationMs: number | null;
  /** LLM 润色耗时（毫秒）：润色/翻译 LLM 调用的 wall clock 时间。 */
  polishDurationMs: number | null;
  dictionaryEntryCount: number | null;
  /** 该会话是否在录音时归档了原始 wav（取决于当时 prefs.recordAudioForDebug）。
   *  true 时前端在 History 渲染播放按钮，凭 id 通过 read_audio_recording IPC 拿字节流。 */
  hasAudioRecording: boolean | null;
  contextCapture: ContextCaptureEntry | null;
}

export type MeetingStatus =
  | 'draft'
  | 'recording'
  | 'paused'
  | 'transcribing_interrupted'
  | 'summarizing'
  | 'summary_failed'
  | 'completed';

export type MeetingAudioState =
  | 'temporary'
  | 'retained'
  | 'pruned'
  | 'missing'
  | 'unavailable';

export type TranscriptSegmentSource = 'realtime_asr' | 'retranscribed_asr';

export type TranscriptTokenKind = 'word' | 'char' | 'token';

export interface TranscriptTokenTimestamp {
  text: string;
  startMs: number;
  endMs: number;
  providerStartMs: number | null;
  providerEndMs: number | null;
  kind: TranscriptTokenKind;
}

export interface TranscriptSegmentMetadata {
  providerId: string | null;
  providerSessionId: string | null;
  providerSegmentId: string | null;
  sentenceId: string | null;
  sequence: number | null;
  audioPartIndex: number | null;
  sessionStartMs: number | null;
  providerStartMs: number | null;
  providerEndMs: number | null;
  tokenTimestamps: TranscriptTokenTimestamp[];
}

export interface TranscriptSegment {
  id: string;
  speakerLabel: string;
  startMs: number;
  endMs: number | null;
  text: string;
  source: TranscriptSegmentSource;
  metadata?: TranscriptSegmentMetadata | null;
}

export interface MeetingTodo {
  id: string;
  content: string;
  owner: string | null;
  dueDate: string | null;
  sourceSegmentIds: string[];
  sourceQuote: string | null;
}

export interface MeetingSummary {
  overview: string;
  keyDecisions: string[];
  todos: MeetingTodo[];
  risksAndOpenQuestions: string[];
}

export interface MeetingAudioMeta {
  state: MeetingAudioState;
  retained: boolean;
  path: string | null;
}

export interface MeetingRecord {
  id: string;
  title: string;
  status: MeetingStatus;
  startedAt: string;
  endedAt: string | null;
  durationMs: number | null;
  transcriptSegments: TranscriptSegment[];
  summary: MeetingSummary;
  audio: MeetingAudioMeta;
  createdAt: string;
  updatedAt: string;
}

export interface MeetingListItem {
  id: string;
  title: string;
  status: MeetingStatus;
  startedAt: string;
  endedAt: string | null;
  durationMs: number | null;
  summaryOverview: string;
  transcriptPreview: string;
  transcriptSegmentCount: number;
  audio: MeetingAudioMeta;
  createdAt: string;
  updatedAt: string;
}

export type MeetingRecordingPhase =
  | 'starting'
  | 'recording'
  | 'paused'
  | 'stopping'
  | 'transcribing_interrupted';

export interface MeetingRecordingSnapshot {
  meeting: MeetingRecord;
  phase: MeetingRecordingPhase;
  elapsedMs: number;
  activeAsrProvider: string;
  activeProviderSessionId?: string | null;
  asrInterrupted: boolean;
}

export interface MeetingAudioLevelEvent {
  meetingId: string;
  level: number;
}

export interface MeetingCompanionPosition {
  x: number;
  y: number;
  monitorId: string | null;
}

export interface MeetingCompanionOpenMeetingEvent {
  meetingId: string;
}

export interface MeetingTranscriptSegmentEvent {
  meetingId: string;
  segment: TranscriptSegment;
}

export interface MeetingTranscriptDraftEvent {
  meetingId: string;
  providerId: string;
  providerSessionId: string | null;
  text: string;
  startMs: number | null;
  endMs: number | null;
  sequence: number | null;
  clear: boolean;
}

export interface MeetingErrorEvent {
  meetingId: string | null;
  code: string;
  message: string;
}

export interface MeetingSummaryEvent {
  meetingId: string;
  status: MeetingStatus;
  meeting: MeetingRecord | null;
  error: MeetingErrorEvent | null;
}

export type MeetingCloseRequestIntent = 'hide' | 'exit';

export interface MeetingCloseRequestEvent {
  snapshot: MeetingRecordingSnapshot;
  intent: MeetingCloseRequestIntent;
}

export function normalizeMeetingCloseRequest(
  payload: MeetingCloseRequestEvent | MeetingRecordingSnapshot,
): MeetingCloseRequestEvent {
  if ('snapshot' in payload) return payload;
  return { snapshot: payload, intent: 'hide' };
}

export interface DictionaryEntry {
  id: string;
  phrase: string;
  note: string | null;
  enabled: boolean;
  hits: number;
  createdAt: string;
}

export interface CorrectionRule {
  id: string;
  pattern: string;
  replacement: string;
  enabled: boolean;
  createdAt: string;
}

export interface VocabPreset {
  id: string;
  name: string;
  phrases: string[];
}

export interface VocabPresetStore {
  custom: VocabPreset[];
  overrides: VocabPreset[];
  disabledBuiltinPresetIds: string[];
}

export type HotkeyTrigger =
  | 'rightOption'
  | 'leftOption'
  | 'rightControl'
  | 'leftControl'
  | 'rightCommand'
  | 'leftCommand'
  | 'leftShift'
  | 'rightShift'
  | 'fn'
  | 'rightAlt'
  | 'mediaPlayPause'
  | 'custom';

export type HotkeyMode = 'toggle' | 'hold' | 'doubleClick';

export interface HotkeyKey {
  code: string;
}

export interface HotkeyBinding {
  trigger: HotkeyTrigger;
  mode: HotkeyMode;
  keys?: HotkeyKey[] | null;
}

export type HotkeyAdapterKind = 'macEventTap' | 'windowsLowLevel' | 'fcitx5' | 'unavailable';

export interface HotkeyCapability {
  adapter: HotkeyAdapterKind;
  availableTriggers: HotkeyTrigger[];
  requiresAccessibilityPermission: boolean;
  supportsModifierOnlyTrigger: boolean;
  supportsSideSpecificModifiers: boolean;
  explicitFallbackAvailable: boolean;
  statusHint: string | null;
}

export interface HotkeyInstallError {
  code: string;
  message: string;
}

export type HotkeyStatusState = 'starting' | 'installed' | 'failed';

export interface HotkeyStatus {
  adapter: HotkeyAdapterKind;
  state: HotkeyStatusState;
  message: string | null;
  lastError: HotkeyInstallError | null;
}

export interface ShortcutBinding {
  /** 主键，例如 "D" / "Space" / "F1" / "RightOption" / "LeftShift" */
  primary: string;
  /** 修饰符：泛化 tag（cmd/ctrl/…）或侧别 tag（cmd-left/ctrl-right/…）。 */
  modifiers: string[];
}

/** 划词语音问答快捷键绑定。null 表示未启用。详见 issue #118。 */
export type QaHotkeyBinding = ShortcutBinding;

/** 自定义录音组合键绑定。当 hotkey.trigger == 'custom' 时使用。 */
export type ComboBinding = ShortcutBinding;

export type CodingAgentProviderId = "claude-code-cli" | "opencode-cli";
export type CodingAgentPermissionMode =
  | "plan"
  | "default"
  | "acceptEdits"
  | "bypassPermissions";

/** 模拟粘贴时按下的快捷键。仅 Windows/Linux 生效；macOS 走 AX 直写。
 *  - ctrlV       : 标准粘贴（默认；大多数编辑器、浏览器、IDE）
 *  - ctrlShiftV  : kitty / alacritty / wezterm / gnome-terminal / foot 等终端
 *  - shiftInsert : xterm / urxvt 等老派 X11 终端
 *  详见 issue #360。 */
export type PasteShortcut = 'ctrlV' | 'ctrlShiftV' | 'shiftInsert';

/** Windows 听写文本插入策略。 */
export type WindowsInsertionMode = 'tsf' | 'sendInput' | 'paste';

/** Windows SendInput 路径的换行模拟方式。 */
export type WindowsSendInputNewlineMode = 'enter' | 'shiftEnter' | 'crlf';

export type WindowsImeInstallState =
  | 'installed'
  | 'notInstalled'
  | 'registrationBroken'
  | 'notWindows';

export interface WindowsImeStatus {
  state: WindowsImeInstallState;
  usingTsfBackend: boolean;
  message: string;
  dllPath: string | null;
}

/** 后台自动更新渠道。stable = 查正式版 manifest（默认）；beta = 查
 *  latest-android-{arch}-beta.json。手动「检查正式版/Beta 更新」按钮不受此字段影响。 */
export type UpdateChannel = 'stable' | 'beta';

export type ThemeMode = 'system' | 'light' | 'dark';

export type MeetingAsrMode = 'inherit_global' | 'provider_specific';

export type MeetingVadSilencePreset = 'short' | 'standard' | 'long';

export interface MeetingAsrSettings {
  mode: MeetingAsrMode;
  providerId: string | null;
  modelOverride: string | null;
  modelProviderId: string | null;
  silencePreset: MeetingVadSilencePreset;
}

export interface CustomStylePrompts {
  raw: string;
  light: string;
  structured: string;
  formal: string;
}

export interface StyleSystemPrompts {
  raw: string;
  light: string;
  structured: string;
  formal: string;
}

export type StylePackKind = 'builtin' | 'imported';

export interface StylePackExample {
  title?: string | null;
  input: string;
  output: string;
}

export interface StylePack {
  id: string;
  name: string;
  description: string;
  author?: string | null;
  version: string;
  kind: StylePackKind;
  baseMode: PolishMode;
  prompt: string;
  examples: StylePackExample[];
  tags: string[];
  iconPath?: string | null;
  createdAt?: string | null;
  updatedAt?: string | null;
  enabled: boolean;
  active: boolean;
  recommendedModel?: string | null;
  compatibleAppVersion?: string | null;
  /** 衍生关系：null = 本地原创（或还没首发到云端）；非空 = 这份 pack 安装自云端 originPackId。 */
  originPackId?: string | null;
  originAuthorLogin?: string | null;
  /** 风格包用途：voice（语音）或 rewrite（重写）。旧包缺省时视为 'voice'。 */
  scope?: StylePackScope;
}

export type StylePackScope = 'voice' | 'rewrite';

export interface StylePackRuntimeDiagnostics {
  packId: string;
  packName: string;
  packPrompt: string;
  packPromptChars: number;
  contextPremise: string;
  contextPremiseChars: number;
  hotwordBlock: string;
  hotwordBlockChars: number;
  historyInstruction: string;
  historyInstructionChars: number;
  singleTurnPrompt: string;
  singleTurnPromptChars: number;
  multiTurnPrompt: string;
  multiTurnPromptChars: number;
  workingLanguages: string[];
  hotwords: string[];
  contextWindowMinutes: number;
  includesContextPremise: boolean;
  includesHotwordBlock: boolean;
  includesHistoryInstruction: boolean;
  previewOmitsFrontApp: boolean;
}

export interface UserPreferences {
  hotkey: HotkeyBinding;
  dictationHotkey: ShortcutBinding;
  defaultMode: PolishMode;
  enabledModes: PolishMode[];
  activeStylePackId: string;
  styleSystemPrompts: StyleSystemPrompts;
  customStylePrompts: CustomStylePrompts;
  launchAtLogin: boolean;
  showCapsule: boolean;
  /** 录音期间临时静音系统输出，停止/取消/出错后恢复原静音状态。 */
  muteDuringRecording: boolean;
  /** 按下录音热键进入 recording 状态时，播放一段合成提示音提醒「已开始录音」。
   *  默认开启；在 capsule 窗口用 Web Audio API 合成，不依赖 showCapsule。 */
  audioCueOnRecord: boolean;
  /** 录音输入设备名称。空字符串 = 使用系统默认麦克风。 */
  microphoneDeviceName: string;
  activeAsrProvider: string;
  meetingAsr: MeetingAsrSettings;
  activeLlmProvider: string;
  /** LLM 思考模式开关。默认关闭；OpenAI 普通 chat 模型会跳过不支持的字段。详见 issue #402。 */
  llmThinkingEnabled: boolean;
  /** 仅 Windows/Linux：粘贴成功后是否恢复用户原剪贴板。默认 true。详见 issue #111。 */
  restoreClipboardAfterPaste: boolean;
  /** 仅 Windows/Linux：模拟粘贴时按下的快捷键。详见 issue #360：kitty/alacritty
   *  等终端只接受 Ctrl+Shift+V，硬编码 Ctrl+V 会被吞掉，听写文本只剩在剪贴板里。
   *  macOS 走 AX 直写不受影响。默认 'ctrlV' 与历史行为一致。 */
  pasteShortcut: PasteShortcut;
  /** Windows：TSF 失败后是否允许快捷键粘贴 / 剪贴板兜底。仅在剪贴板写失败时才再试 SendInput。关闭后可验证是否真实 TSF 上屏。 */
  allowNonTsfInsertionFallback: boolean;
  /** Windows：听写插入策略（TSF / SendInput / 剪贴板粘贴）。 */
  windowsInsertionMode: WindowsInsertionMode;
  /** Windows SendInput 路径的换行模拟方式。 */
  windowsSendInputNewlineMode: WindowsSendInputNewlineMode;
  /** 旧版兼容：`true` 等价于 `windowsInsertionMode === 'sendInput'`。 */
  windowsSendInputInsertionOnly: boolean;
  /** Windows：SendInput 模式下是否在系统键盘列表（Win+Space）中显示 OpenLess。 */
  windowsShowOpenlessInKeyboardList: boolean;
  /** 用户的工作语言（多选，原生名）；作为前提注入 LLM polish/translate prompt 头部。 */
  workingLanguages: string[];
  /** 翻译模式目标语言（单选，原生名）；空串 = 不启用 Shift 翻译。详见 issue #4。 */
  translationTargetLanguage: string;
  /** 中文输出字形偏好：由界面语言（简/繁）自动同步，不单独暴露设置项。 */
  chineseScriptPreference: 'auto' | 'simplified' | 'traditional';
  /** 最终输出语言偏好：由界面语言自动同步，不单独暴露设置项。 */
  outputLanguagePreference: 'auto' | 'zhCn' | 'zhTw' | 'en' | 'ja' | 'ko';
  /** 划词语音问答快捷键。null = 未启用。详见 issue #118。 */
  qaHotkey: QaHotkeyBinding | null;
  /** 是否把 Q&A 历史写到本地存档。详见 issue #118。 */
  qaSaveHistory: boolean;
  /** 自定义录音组合键。当 hotkey.trigger == 'custom' 时使用。null = 未设置。 */
  customComboHotkey: ComboBinding | null;
  /** 录音中触发翻译的全局快捷键。默认 Shift。 */
  translationHotkey: ShortcutBinding;
  /** 切换到上一个润色风格的全局快捷键。null = 用户已停用（issue #576）。 */
  switchStyleHotkey: ShortcutBinding | null;
  /** 打开 OpenLess 主窗口的全局快捷键。null = 用户已停用（issue #576）。 */
  openAppHotkey: ShortcutBinding | null;
  /** 文本重写全局快捷键。null = 停用。默认 Ctrl/Cmd+Shift+R。 */
  rewriteHotkey: ShortcutBinding | null;
  /** 截图记录全局快捷键。功能默认关闭；开启后默认 Enter。 */
  screenshotRecordHotkey: ShortcutBinding | null;
  /** 截图记录开关。默认关闭，用户主动开启。 */
  screenshotRecordEnabled: boolean;
  /** 截图记录暂停状态。不同于关闭，可从托盘快速恢复。 */
  screenshotRecordPaused: boolean;
  /** 单条截图记录合并窗口秒数。V1 默认 60。 */
  screenshotRecordMergeWindowSeconds: number;
  /** 单条截图记录最多提交给分析模型的截图数。V1 默认 5。 */
  screenshotRecordMaxImagesPerAnalysis: number;
  /** 统一上下文分析完整摘要提示词。影响语音、重写、截图记录的完整摘要。 */
  contextAnalysisFullSummaryPrompt: string | null;
  /** 每日定时生成日报开关。默认关闭。 */
  dailyReportScheduleEnabled: boolean;
  /** 每日定时生成日报时间，格式 HH:mm。默认 18:00。 */
  dailyReportScheduleTime: string;
  selectedDailyReportTemplateId: string;
  selectedWeeklyReportTemplateId: string;
  selectedMonthlyReportTemplateId: string;
  /** 是否保存重写历史。默认 true。 */
  rewriteSaveHistory: boolean;
  /** 文本重写当前激活的风格包 ID。null = 使用内置默认 (builtin.rewrite)。 */
  activeRewriteStylePackId?: string | null;
  /** Less Computer：是否启用。默认关闭。 */
  codingAgentEnabled: boolean;
  /** Agent 后端：claude-code-cli（默认）/ opencode-cli。 */
  codingAgentProvider: CodingAgentProviderId;
  /** Agent 模型，null = 运行时取便宜默认（sonnet）。 */
  codingAgentModel: string | null;
  /** 权限模式：plan/default/acceptEdits/bypassPermissions。 */
  codingAgentPermissionMode: CodingAgentPermissionMode;
  /** Agent 工作目录，null = 临时目录。 */
  codingAgentWorkdir: string | null;
  /** Agent 可执行文件路径/命令，null 或空 = 按后端取默认（claude / opencode）。 */
  codingAgentExe: string | null;
  /** Less Computer 按住说话快捷键。null = 停用；目前仅 macOS 显示/生效。 */
  codingAgentVoiceHotkey: ShortcutBinding | null;
  /** 热键 1：语音 Agent 面板键。null = 停用。 */
  codingAgentPanelHotkey: ShortcutBinding | null;
  /** 热键 2：快取用键（选中→Claude→回插）。null = 未配置。 */
  codingAgentQuickHotkey: ShortcutBinding | null;
  /** 本地 Qwen3-ASR 当前激活的模型 id。仅在 activeAsrProvider === 'local-qwen3' 时有意义。 */
  localAsrActiveModel: string;
  /** 本地模型下载源镜像（'huggingface' / 'hf-mirror'）。 */
  localAsrMirror: string;
  /** 本地 ASR 引擎在内存中的保留时长（秒）。0 = 说完话即释放；
   *  300 = 默认 5 分钟；86400 ≈ 不释放（保持加载）。 */
  localAsrKeepLoadedSecs: number;
  /** Windows Foundry Local Whisper 当前激活的模型 alias。 */
  foundryLocalAsrModel: string;
  /** Windows Foundry Local native runtime 下载源。 */
  foundryLocalRuntimeSource: string;
  /** Windows Foundry Local Whisper 语言 hint。空字符串表示自动检测。 */
  foundryLocalAsrLanguageHint: string;
  /** Windows Foundry Local Whisper 模型在 runtime 中保持加载的秒数。 */
  foundryLocalAsrKeepLoadedSecs: number;
  /** Windows sherpa-onnx 本地 ASR 当前激活的模型 alias。 */
  sherpaOnnxModel: string;
  /** Windows sherpa-onnx 语言 hint。空字符串表示自动检测。 */
  sherpaOnnxLanguageHint: string;
  /** Windows sherpa-onnx 模型在 runtime 中保持加载的秒数。 */
  sherpaOnnxKeepLoadedSecs: number;
  /** 历史记录保留天数。0 = 不按时间清理（仍受 200 条上限）。默认 7。 */
  historyRetentionDays: number;
  /** Meeting raw audio retention count. 0 disables long-term audio retention; default 20; max 100. */
  meetingAudioRetentionCount: number;
  /** 会议桌宠可选开关，默认关闭。 */
  meetingCompanionEnabled: boolean;
  /** 是否锁定桌宠位置，默认关闭。 */
  meetingCompanionPositionLocked: boolean;
  /** 最近一次合法窗口位置；null 表示尚未摆放。 */
  meetingCompanionPosition: MeetingCompanionPosition | null;
  /** 对话感知 polish 上下文窗口（分钟）。0 = 关闭。默认 5。详见 PR-A。 */
  polishContextWindowMinutes: number;
  /** 启动时静默运行（不弹主窗口）。Windows 开机自启场景常用——只想要后台 + 托盘，
   *  不想被主窗口打扰。开后所有启动路径都不弹窗，从菜单栏 / 托盘进入主窗口。默认 false。 */
  startMinimized: boolean;
  /** UI theme preference: follow OS, light, or dark. */
  themeMode: ThemeMode;
  /** 后台自动更新渠道。stable（默认）= AutoUpdateGate 查正式版 manifest；
   *  beta = 查 Beta manifest。About / Advanced 的手动检查按钮各自固定 stable/beta。 */
  updateChannel: UpdateChannel;
  /** 流式输入：润色 SSE 一边到达一边逐字模拟键盘事件输出到当前焦点。开启后用户感知到
   *  的处理时延显著降低。v1 限定 macOS + OpenAI-compatible provider，其他配置自动回落
   *  到原一次性插入。默认 true。 */
  streamingInsert: boolean;
  /** issue #440 一次性迁移标记：旧配置缺少该字段时后端会把老默认 false 迁到 true；
   *  迁移后用户再手动关掉 streamingInsert 时保留 false。 */
  streamingInsertDefaultMigrated: boolean;
  /** 流式输入成功后是否把最终润色文本写回剪贴板。开启后 Cmd+V 还能重复粘贴该次输出，
   *  与一次性路径行为对齐。默认 true。 */
  streamingInsertSaveClipboard: boolean;
  /** 概览页是否显示「年度活动」热力图卡。默认 true；关闭只隐藏卡片，活动计数照常记录。 */
  showOverviewActivityHeatmap: boolean;
  /** 主窗口启动 + 后台每 60 分钟自动检查更新。默认 true。
   *  Android：开启后自动检查并下载，校验后打开系统安装器。
   *  桌面：开启后自动检查，发现更新弹窗由用户确认安装。
   *  关闭后仅 Settings 手动「检查更新」按钮可用。 */
  autoUpdateCheck: boolean;
  /** 历史记录上限（条数）。null = 走默认 2000；5..=10000 之间为用户自定义。 */
  historyMaxEntries: number | null;
  /** 是否为每次会话保留原始麦克风音频文件（wav），用于排查 ASR 误识别 / 麦克风灵敏度。
   *  默认 false。开启后会占磁盘空间，受 historyRetentionDays 同样的清理策略约束。 */
  recordAudioForDebug: boolean;
  /** recordings/ 里保留的最近 wav 文件数。null = 跟随 200 硬上限；1..=200 之间为用户自定义。
   *  跟 historyMaxEntries 解耦——「文本档案多但 wav 只留最近 5 条」是合法组合。 */
  audioRecordingMaxEntries: number | null;
  contextCaptureEnabled: boolean;
  contextVisionAnalysisEnabled: boolean;
  contextVisionAnalysisConsentAccepted: boolean;
  /** Marketplace HTTP 基地址。空 = 本地开发默认 http://127.0.0.1:8090；生产填 https://api.<domain>。 */
  marketplaceBaseUrl: string;
  /** Marketplace dev-mode 模拟登录用户名（GitHub login 风格）。生产换 OAuth token 后此字段废弃。 */
  marketplaceDevLogin: string;
  /** 是否启用远程输入（局域网手机录音）HTTPS+WS 服务。默认 false。 */
  remoteInputEnabled: boolean;
  /** 远程输入服务监听端口（HTTPS）。默认 8443。 */
  remoteInputPort: number;
  /** 远程输入配对码（6 位数字）。空 = server 首次启动时随机生成。 */
  remoteInputPin: string;
  /** 手机录音页默认交互方式：'toggle'（点击切换）/ 'hold'（按住说话）。 */
  remoteInputDefaultMode: 'toggle' | 'hold';
  /** Android: cross-app dictation insert strategy. */
  androidInsertStrategy: AndroidInsertStrategy;
  /** Android: floating overlay visibility trigger mode. */
  androidOverlayTrigger: AndroidOverlayTrigger;
  /** Android: how the floating overlay enters the armed interaction state. */
  androidOverlayActivationMode: AndroidOverlayActivationMode;
  /** Android: action performed by left swiping while the overlay is armed. */
  androidOverlayLeftSwipeAction: AndroidOverlayLeftSwipeAction;
  /** Android: vertical swipe direction that cancels recording. */
  androidOverlayCancelSwipeDirection: AndroidOverlayCancelSwipeDirection;
  /** Android: floating overlay control diameter in dp. */
  androidOverlaySizeDp: number;
  /** 截图白名单开关。默认 true（开启后只记录白名单应用的截图）。 */
  screenshotWhitelistEnabled: boolean;
  /** 截图白名单应用列表。默认包含常用工作应用。 */
  screenshotWhitelistApps: ScreenshotWhitelistApp[];
  /** 默认白名单版本号。升级时用以判断是否需要补充新增默认项。 */
  screenshotWhitelistDefaultsVersion: number;
  /** 按应用聚合分析开关。默认关闭，作为高级选项。 */
  screenshotAppAggregationEnabled: boolean;
}

export interface MarketplaceListItem {
  id: string;
  slug: string;
  name: string;
  description: string;
  authorLogin: string;
  version: string;
  baseMode: PolishMode;
  tags: string[];
  likeCount: number;
  downloadCount: number;
  publishedAt: string;
  updatedAt: string;
  /** 衍生关系：null = 原创；非空 = 衍生自 originPackId，UI 显「衍生自 @originAuthorLogin」。 */
  originPackId?: string | null;
  originAuthorLogin?: string | null;
}

export interface MarketplaceDetail extends MarketplaceListItem {
  prompt: string;
  state: 'pending' | 'approved' | 'rejected';
}

export interface MarketplaceMyPackItem extends MarketplaceListItem {
  state: 'pending' | 'approved' | 'rejected' | 'withdrawn' | 'superseded' | string;
}

export interface MicrophoneDevice {
  name: string;
  isDefault: boolean;
}

/** Rust 通过 `qa:state` 事件下发的 payload。
 *  v2 (issue #118 v2)：支持多轮对话，messages 数组每次由后端整段下发（单一可信源）。
 *  v2.1：开 `stream:true`，LLM 答案逐 chunk 通过 `answer_delta` 事件推前端边渲染。 */
export type QaStateKind =
  | 'idle'
  | 'recording'
  | 'loading'
  | 'thinking'
  | 'answer_delta'
  | 'answer'
  | 'error';

export interface QaChatMessage {
  role: 'user' | 'assistant';
  content: string;
}

export interface QaStatePayload {
  kind: QaStateKind;
  /** 后端权威：当前已有的多轮对话历史（user → assistant 交替）。answer 事件带完整版。 */
  messages?: QaChatMessage[];
  /** recording 状态时附带的选区预览（前 60 字）。 */
  selection_preview?: string | null;
  /** error 状态时附带的提示。 */
  error?: string;
  /** answer_delta 事件时附带的本帧增量字符串。 */
  chunk?: string;
}

/**
 * Less Computer 语音 Agent 浮窗事件（窗口 label = "less-computer"，事件名
 * `less-computer:event`）。后端按 `kind` 标记，前端据此把交互渲染成聊天结构。
 */
export type LessComputerEvent = (
  /** 一轮用户气泡（语音指令转写）。fresh=true 表示新会话（清空历史）；否则追加为后续轮次。 */
  | { kind: 'user'; text: string; fresh?: boolean }
  /** Agent 启动，进入运行态。 */
  | { kind: 'started' }
  /** 流式回复增量（来自 CodingAgentEvent::Delta）。 */
  | { kind: 'delta'; text: string }
  /** 工具调用提示（来自 CodingAgentEvent::ToolUse，如 "Bash"）。 */
  | { kind: 'tool'; name: string }
  /** 会话上下文被压缩（来自 CodingAgentEvent::Compaction），输出流对应位置内嵌提示。 */
  | { kind: 'compaction' }
  /** 内联审批卡：高风险动作被护栏拦下，等用户 Approve / Deny。 */
  | { kind: 'approval'; token: string; command: string; reason: string }
  /** 运行完成：最终结果 + 成本（美元）。 */
  | { kind: 'completed'; text: string; costUsd?: number | null }
  /** 用户从胶囊取消正在运行的 Agent。 */
  | { kind: 'cancelled' }
  /** 运行出错。 */
  | { kind: 'error'; message: string }
) & {
  /** 单调事件序号（后端 emit 时编）。用于 less_computer_sync 重放与实时流去重；
   *  缓冲锁异常时后端可能省略，无 seq 的事件前端无条件应用。 */
  seq?: number;
};

/** 内置语言列表 — 前端 Settings UI 用，后端只接收原生名字符串拼 prompt。
 *  添加新语言时直接在这里加一项（原生名），无需修改后端。 */
export const SUPPORTED_LANGUAGES: readonly string[] = [
  '简体中文',
  '繁体中文',
  'English',
  '日本語',
  '한국어',
  'Français',
  'Deutsch',
  'Español',
  'Italiano',
  'Português',
  'Русский',
  'العربية',
  'Tiếng Việt',
  'ไทย',
  'हिन्दी',
] as const;

export type CapsuleState =
  | 'idle'
  | 'recording'
  | 'transcribing'
  | 'polishing'
  | 'done'
  | 'cancelled'
  | 'error';

export interface CapsulePayload {
  state: CapsuleState;
  level: number; // 0..1 RMS
  elapsedMs: number;
  message: string | null;
  insertedChars: number | null;
  /** 当前 session 是否处于翻译模式（用户已按过 Shift）。详见 issue #4。 */
  translation: boolean;
  /** 当前是否是 Less Computer 会话：处理态文案显示 "using" 而非 "thinking"。 */
  operating?: boolean;
  /** 当前胶囊状态来自文本重写流程。隐藏取消/确认按钮。 */
  rewrite?: boolean;
  /**
   * 预备态：胶囊已「乐观显示」（按下热键即弹出并播入场动画），但麦克风还没吐第一帧
   * PCM。为 true 时录音光条渲染成「待命」形态（柔和呼吸、不接真实电平），暗示用户稍候
   * 再开口；麦克风就绪后翻 false，光条点亮进入正式录音。只对 recording 有意义。
   */
  warming?: boolean;
}

export interface CredentialsStatus {
  activeAsrProvider: string;
  activeLlmProvider: string;
  asrConfigured: boolean;
  llmConfigured: boolean;
  /** 兼容旧字段（过渡期保留）。 */
  volcengineConfigured: boolean;
  arkConfigured: boolean;
}

export interface AsrProviderCapabilities {
  providerId: string;
  supportsRealtime: boolean;
  supportsDraftResult: boolean;
  supportsFinalSegmentEvent: boolean;
  supportsBatchResult: boolean;
  supportsServerVad: boolean;
  supportsVadSilencePreset: boolean;
  supportsSentenceTimestamp: boolean;
  supportsWordTimestamp: boolean;
  supportsPunctuation: boolean;
  supportsItn: boolean;
}

export interface TodayMetrics {
  charsToday: number;
  segmentsToday: number;
  avgLatencyMs: number;
  totalDurationMs: number;
}

export type PermissionStatus =
  | 'granted'
  | 'denied'
  | 'notDetermined'
  | 'restricted'
  | 'notApplicable';

/** Runtime platform kind returned by `get_platform_capabilities`. */
export type PlatformKind = 'desktop' | 'android' | 'mobile';

/** Feature flags for desktop vs Android APK UI gating. Mirrors src-tauri PlatformCapabilities. */
export interface PlatformCapabilities {
  platform: PlatformKind;
  supportsDesktopHotkey: boolean;
  supportsTray: boolean;
  supportsOverlay: boolean;
  supportsImeInput: boolean;
  supportsLocalAsr: boolean;
  supportsInAppDictation: boolean;
  supportsAutoUpdate: boolean;
}
