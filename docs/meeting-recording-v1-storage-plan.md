# Meeting Recording V1 Storage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the storage foundation for Meeting Recording V1: typed meeting records, local JSON persistence, meeting-audio retention, and IPC accessors.

**Architecture:** Add meeting-specific types beside existing IPC value types, and add a focused `MeetingStore` under `persistence/` rather than extending short-dictation `HistoryStore`. Meeting audio uses a separate `meeting-recordings/` directory so short-dictation retention cannot accidentally prune meeting audio. Frontend receives TypeScript mirror types and IPC wrappers, but no UI is implemented in this plan.

**Tech Stack:** Tauri 2, Rust, Serde JSON, chrono, uuid, React/TypeScript IPC wrappers.

---

## Scope

This plan implements **V1-1 会议基础数据与本地存储** only.

In scope:

- Meeting data types.
- Local JSON store.
- IPC commands for list/get/create/update/delete.
- Meeting audio path helpers.
- Meeting audio retention count setting.
- Tests for persistence and retention.

Out of scope:

- Recording.
- ASR.
- UI.
- LLM summary generation.
- VAD（Voice Activity Detection，语音活动检测）.
- speaker diarization（说话人分离）.
- system audio capture（系统声音采集）.

## File Structure

Create:

- `openless-all/app/src-tauri/src/persistence/meeting.rs`
  - Owns `MeetingStore`, `meetings.json`, list/get/create/update/delete, and meeting-audio retention.
- `openless-all/app/src-tauri/src/commands/meetings.rs`
  - Tauri IPC commands for the meeting store.
- `openless-all/app/src/lib/ipc/meetings.ts`
  - Frontend IPC wrappers.

Modify:

- `openless-all/app/src-tauri/src/types.rs`
  - Add meeting value types and `meeting_audio_retention_count` preference.
- `openless-all/app/src-tauri/src/persistence/mod.rs`
  - Export `meeting` module.
- `openless-all/app/src-tauri/src/persistence/paths.rs`
  - Add meeting recording path helpers.
- `openless-all/app/src-tauri/src/commands/mod.rs`
  - Import/re-export meeting commands and meeting types.
- `openless-all/app/src-tauri/src/lib.rs`
  - Register meeting commands in desktop and mobile invoke handlers.
- `openless-all/app/src/lib/types.ts`
  - Add TypeScript mirror types and `meetingAudioRetentionCount` preference.
- `openless-all/app/src/lib/ipc/index.ts`
  - Export meeting IPC wrappers.
- `openless-all/app/src/lib/ipc/mock-data.ts`
  - Add mock meeting records and default preference value.

Do not modify:

- Existing dictation `HistoryStore` behavior.
- Existing `recordings/` debug audio retention behavior.
- Existing ASR or coordinator flows.

## Data Contract

Add Rust types using `#[serde(rename_all = "camelCase")]`, mirrored exactly in TypeScript.

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStatus {
    Draft,
    Recording,
    Paused,
    TranscribingInterrupted,
    Summarizing,
    SummaryFailed,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MeetingAudioState {
    Temporary,
    Retained,
    Pruned,
    Missing,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptSegmentSource {
    RealtimeAsr,
    RetranscribedAsr,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegment {
    pub id: String,
    pub speaker_label: String,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub text: String,
    pub source: TranscriptSegmentSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MeetingTodo {
    pub id: String,
    pub content: String,
    pub owner: Option<String>,
    pub due_date: Option<String>,
    pub source_segment_ids: Vec<String>,
    pub source_quote: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct MeetingSummary {
    pub overview: String,
    pub key_decisions: Vec<String>,
    pub todos: Vec<MeetingTodo>,
    pub risks_and_open_questions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MeetingAudioMeta {
    pub state: MeetingAudioState,
    pub retained: bool,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MeetingRecord {
    pub id: String,
    pub title: String,
    pub status: MeetingStatus,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub transcript_segments: Vec<TranscriptSegment>,
    pub summary: MeetingSummary,
    pub audio: MeetingAudioMeta,
    pub created_at: String,
    pub updated_at: String,
}
```

Use `Draft` for records created before V1-2 recording exists. V1-2 can transition records into `Recording` / `Paused` / `Completed`.

Add preference:

```rust
#[serde(default = "default_meeting_audio_retention_count")]
pub meeting_audio_retention_count: u32,

fn default_meeting_audio_retention_count() -> u32 {
    20
}

pub fn clamp_meeting_audio_retention_count(value: u32) -> u32 {
    value.min(100)
}
```

`0` means no long-term retained meeting audio. The setting is still stored as a number, not an optional field.

## IPC Contract

Create commands:

```rust
#[tauri::command]
pub fn list_meetings() -> Result<Vec<MeetingRecord>, String>

#[tauri::command]
pub fn get_meeting(id: String) -> Result<MeetingRecord, String>

#[tauri::command]
pub fn create_meeting_record(record: MeetingRecord) -> Result<MeetingRecord, String>

#[tauri::command]
pub fn update_meeting_record(record: MeetingRecord) -> Result<MeetingRecord, String>

#[tauri::command]
pub fn delete_meeting_record(id: String) -> Result<(), String>
```

V1-1 intentionally accepts whole `MeetingRecord` values for create/update. Field-specific commands can be introduced in V1-3 when UI editing semantics are known.

Validate meeting ids with the existing UUID literal rule used for session ids. Return `"invalid meeting id"` for invalid ids.

## Task 1: Add Meeting Types And Preference

**Files:**

- Modify: `openless-all/app/src-tauri/src/types.rs`
- Modify: `openless-all/app/src/lib/types.ts`
- Modify: `openless-all/app/src/lib/ipc/mock-data.ts`

- [ ] **Step 1: Add Rust meeting types to `types.rs`**

Place the new meeting structs near `DictationSession` so IPC value types stay grouped. Use the exact data contract above.

- [ ] **Step 2: Add meeting audio retention preference to `UserPreferences`**

Add `meeting_audio_retention_count` near existing history/audio preferences in `UserPreferences`, `UserPreferencesWire`, `From<UserPreferences> for UserPreferencesWire`, `TryFrom<UserPreferencesWire> for UserPreferences`, and `Default for UserPreferences`.

Expected default:

```rust
meeting_audio_retention_count: default_meeting_audio_retention_count(),
```

Expected deserialization behavior:

- Missing field becomes `20`.
- Explicit `0` stays `0`.
- Explicit `150` is clamped to `100` when loaded into `UserPreferences`.

- [ ] **Step 3: Add Rust tests for preference defaults and clamp**

Add tests in the existing `#[cfg(test)] mod tests` in `types.rs`:

```rust
#[test]
fn meeting_audio_retention_count_defaults_to_twenty() {
    let prefs = UserPreferences::default();
    assert_eq!(prefs.meeting_audio_retention_count, 20);

    let from_empty: UserPreferences = serde_json::from_str("{}").unwrap();
    assert_eq!(from_empty.meeting_audio_retention_count, 20);
}

#[test]
fn meeting_audio_retention_count_allows_zero_and_clamps_upper_bound() {
    let zero: UserPreferences =
        serde_json::from_str(r#"{"meetingAudioRetentionCount":0}"#).unwrap();
    assert_eq!(zero.meeting_audio_retention_count, 0);

    let too_large: UserPreferences =
        serde_json::from_str(r#"{"meetingAudioRetentionCount":150}"#).unwrap();
    assert_eq!(too_large.meeting_audio_retention_count, 100);
}
```

- [ ] **Step 4: Add TypeScript mirror types**

Add these to `openless-all/app/src/lib/types.ts`:

```ts
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

export interface TranscriptSegment {
  id: string;
  speakerLabel: string;
  startMs: number;
  endMs: number | null;
  text: string;
  source: TranscriptSegmentSource;
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
```

Add to `UserPreferences`:

```ts
meetingAudioRetentionCount: number;
```

- [ ] **Step 5: Add mock preference value**

In `openless-all/app/src/lib/ipc/mock-data.ts`, add:

```ts
meetingAudioRetentionCount: 20,
```

to `mockSettings`.

- [ ] **Step 6: Run targeted tests**

Run:

```powershell
cd openless-all/app/src-tauri
cargo test meeting_audio_retention_count
```

Expected: the two new tests pass.

## Task 2: Add Meeting Store

**Files:**

- Create: `openless-all/app/src-tauri/src/persistence/meeting.rs`
- Modify: `openless-all/app/src-tauri/src/persistence/mod.rs`

- [ ] **Step 1: Create `MeetingStore`**

Create `meeting.rs` with the same storage style as `history.rs`, but use `meetings.json`.

Required API:

```rust
pub struct MeetingStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl MeetingStore {
    pub fn new() -> Result<Self>;
    pub(crate) fn new_fallback() -> Self;
    pub fn list(&self) -> Result<Vec<MeetingRecord>>;
    pub fn get(&self, id: &str) -> Result<Option<MeetingRecord>>;
    pub fn create(&self, record: MeetingRecord) -> Result<MeetingRecord>;
    pub fn update(&self, record: MeetingRecord) -> Result<Option<MeetingRecord>>;
    pub fn delete(&self, id: &str) -> Result<Option<MeetingRecord>>;
}
```

Behavior:

- `list()` returns newest-first.
- `create()` inserts at index 0.
- `create()` replaces an existing record with the same id instead of duplicating it.
- `update()` returns `Ok(None)` if the id does not exist.
- `delete()` returns the removed record so command code can delete audio.

- [ ] **Step 2: Export the module**

In `persistence/mod.rs`:

```rust
mod meeting;
pub use meeting::*;
```

- [ ] **Step 3: Add store tests**

Add tests inside `meeting.rs` using a temp path constructor. If the public constructor always uses `data_dir()`, add a test-only constructor:

```rust
#[cfg(test)]
fn new_for_path(path: PathBuf) -> Self {
    Self {
        path,
        lock: Mutex::new(()),
    }
}
```

Tests to add:

```rust
#[test]
fn meeting_store_create_lists_newest_first_and_replaces_duplicate_id()

#[test]
fn meeting_store_update_returns_none_for_missing_record()

#[test]
fn meeting_store_delete_returns_removed_record()
```

Use minimal `MeetingRecord` fixtures with deterministic ids and timestamps.

- [ ] **Step 4: Run store tests**

Run:

```powershell
cd openless-all/app/src-tauri
cargo test meeting_store
```

Expected: all meeting store tests pass.

## Task 3: Add Meeting Audio Path Helpers And Retention

**Files:**

- Modify: `openless-all/app/src-tauri/src/persistence/paths.rs`
- Modify: `openless-all/app/src-tauri/src/persistence/meeting.rs`

- [ ] **Step 1: Add meeting recordings root**

In `paths.rs`, add:

```rust
pub fn meeting_recordings_root() -> Result<PathBuf> {
    let dir = data_dir()?.join("meeting-recordings");
    ensure_dir(&dir)?;
    Ok(dir)
}

pub fn meeting_recording_path_for_id(meeting_id: &str) -> Result<PathBuf> {
    Ok(meeting_recordings_root()?.join(format!("{meeting_id}.wav")))
}
```

Do not reuse `recordings_root()` because that directory is tied to short-dictation debug audio and history retention.

- [ ] **Step 2: Add audio file cleanup on delete**

In `MeetingStore` or `commands/meetings.rs`, delete `meeting-recordings/<id>.wav` when deleting a meeting. Prefer command-level cleanup so the pure store stays JSON-only.

Expected behavior:

- Missing audio file is not an error.
- Other delete errors are returned as `"delete meeting audio failed: ..."` only after the JSON record has been removed. If this happens, the command should surface the error so the user knows cleanup was incomplete.

- [ ] **Step 3: Add retention helper**

Add a helper in `meeting.rs`:

```rust
pub fn prune_meeting_audio(records: &mut [MeetingRecord], retention_count: u32) -> Result<usize>
```

Behavior:

- Clamp `retention_count` to `0..=100`.
- Consider only records whose `audio.state == MeetingAudioState::Retained` and whose audio path/file exists.
- Sort by `ended_at` if present, otherwise `created_at`, newest first.
- Keep newest `retention_count`.
- For pruned records:
  - Delete the audio file if present.
  - Set `audio.state = MeetingAudioState::Pruned`.
  - Set `audio.retained = false`.
  - Set `audio.path = None`.
- Return the number of pruned audio files/records.

Call this helper from a `MeetingStore` method that writes updated records back to `meetings.json`, for example:

```rust
pub fn prune_audio_retention(&self, retention_count: u32) -> Result<usize>
```

- [ ] **Step 4: Add retention tests**

Add tests:

```rust
#[test]
fn prune_meeting_audio_keeps_newest_n_and_preserves_text_records()

#[test]
fn prune_meeting_audio_with_zero_prunes_all_retained_audio()

#[test]
fn prune_meeting_audio_clamps_retention_to_one_hundred()
```

Use temp `meeting-recordings` paths. Do not touch the real app data directory in tests.

- [ ] **Step 5: Run retention tests**

Run:

```powershell
cd openless-all/app/src-tauri
cargo test prune_meeting_audio
```

Expected: all retention tests pass.

## Task 4: Add Meeting IPC Commands

**Files:**

- Create: `openless-all/app/src-tauri/src/commands/meetings.rs`
- Modify: `openless-all/app/src-tauri/src/commands/mod.rs`
- Modify: `openless-all/app/src-tauri/src/lib.rs`

- [ ] **Step 1: Create command file**

Create `commands/meetings.rs`.

Commands:

```rust
#[tauri::command]
pub fn list_meetings() -> Result<Vec<MeetingRecord>, String>

#[tauri::command]
pub fn get_meeting(id: String) -> Result<MeetingRecord, String>

#[tauri::command]
pub fn create_meeting_record(record: MeetingRecord) -> Result<MeetingRecord, String>

#[tauri::command]
pub fn update_meeting_record(record: MeetingRecord) -> Result<MeetingRecord, String>

#[tauri::command]
pub fn delete_meeting_record(id: String) -> Result<(), String>
```

Use `MeetingStore::new()` inside commands for V1-1. Do not add `MeetingStore` to `Coordinator` yet; V1-2 can decide whether meeting sessions need shared state.

Validation:

- Use `is_valid_session_id(&id)` for meeting ids. The name is session-specific but the validation rule is UUID-literal and already used as an IPC boundary helper.
- Invalid ids return `"invalid meeting id"`.
- `get_meeting` and `update_meeting_record` missing records return `"meeting not found"`.

- [ ] **Step 2: Re-export commands**

In `commands/mod.rs`:

```rust
mod meetings;
pub use meetings::*;
```

Also add meeting types to the `pub(crate) use crate::types::{ ... }` list if needed by submodules.

- [ ] **Step 3: Register commands in Tauri handlers**

In `lib.rs`, add these commands to both desktop and mobile handler lists:

```rust
commands::list_meetings,
commands::get_meeting,
commands::create_meeting_record,
commands::update_meeting_record,
commands::delete_meeting_record,
```

Use `$crate::commands::...` forms in the mobile macro.

- [ ] **Step 4: Add command tests for validation helpers**

If command functions are easy to unit test without Tauri state, add tests in `commands/meetings.rs` for invalid ids. If not, keep validation in a pure helper:

```rust
fn validate_meeting_id(id: &str) -> Result<(), String>
```

Tests:

```rust
#[test]
fn validate_meeting_id_rejects_path_traversal()

#[test]
fn validate_meeting_id_accepts_uuid_literal()
```

- [ ] **Step 5: Run command tests**

Run:

```powershell
cd openless-all/app/src-tauri
cargo test meeting_id
```

Expected: meeting id validation tests pass.

## Task 5: Add Frontend IPC Wrappers

**Files:**

- Create: `openless-all/app/src/lib/ipc/meetings.ts`
- Modify: `openless-all/app/src/lib/ipc/index.ts`
- Modify: `openless-all/app/src/lib/ipc/mock-data.ts`

- [ ] **Step 1: Add mock meeting records**

In `mock-data.ts`, export:

```ts
export const mockMeetings: MeetingRecord[] = [
  {
    id: '00000000-0000-4000-8000-000000000001',
    title: '示例会议记录',
    status: 'completed',
    startedAt: new Date(Date.now() - 30 * 60 * 1000).toISOString(),
    endedAt: new Date().toISOString(),
    durationMs: 30 * 60 * 1000,
    transcriptSegments: [
      {
        id: 'seg-1',
        speakerLabel: '未区分',
        startMs: 0,
        endMs: 8000,
        text: '我们先确认 V1 只做会议录音、原文和总结。',
        source: 'realtime_asr',
      },
    ],
    summary: {
      overview: '确认会议录音总结 V1 范围。',
      keyDecisions: ['V1 不包含说话人分离和系统声音采集。'],
      todos: [
        {
          id: 'todo-1',
          content: '完成会议存储计划',
          owner: null,
          dueDate: null,
          sourceSegmentIds: ['seg-1'],
          sourceQuote: '我们先确认 V1 只做会议录音、原文和总结。',
        },
      ],
      risksAndOpenQuestions: [],
    },
    audio: {
      state: 'retained',
      retained: true,
      path: null,
    },
    createdAt: new Date(Date.now() - 30 * 60 * 1000).toISOString(),
    updatedAt: new Date().toISOString(),
  },
];
```

- [ ] **Step 2: Add IPC wrappers**

Create `meetings.ts`:

```ts
import type { MeetingRecord } from "../types";
import { invokeOrMock } from "./shared";
import { mockMeetings } from "./mock-data";

export function listMeetings(): Promise<MeetingRecord[]> {
  return invokeOrMock("list_meetings", undefined, () => mockMeetings);
}

export function getMeeting(id: string): Promise<MeetingRecord> {
  return invokeOrMock(
    "get_meeting",
    { id },
    () => mockMeetings.find(item => item.id === id) ?? mockMeetings[0],
  );
}

export function createMeetingRecord(record: MeetingRecord): Promise<MeetingRecord> {
  return invokeOrMock("create_meeting_record", { record }, () => record);
}

export function updateMeetingRecord(record: MeetingRecord): Promise<MeetingRecord> {
  return invokeOrMock("update_meeting_record", { record }, () => record);
}

export function deleteMeetingRecord(id: string): Promise<void> {
  return invokeOrMock("delete_meeting_record", { id }, () => undefined);
}
```

- [ ] **Step 3: Export wrappers**

In `ipc/index.ts`:

```ts
export {
  listMeetings,
  getMeeting,
  createMeetingRecord,
  updateMeetingRecord,
  deleteMeetingRecord,
} from "./meetings";
```

- [ ] **Step 4: Run TypeScript check**

Run:

```powershell
cd openless-all/app
.\node_modules\.bin\tsc.CMD --noEmit
```

Expected: TypeScript passes with no new type errors.

## Task 6: Final Verification For V1-1

**Files:**

- All files changed in Tasks 1-5.

- [ ] **Step 1: Run Rust targeted tests**

Run:

```powershell
cd openless-all/app/src-tauri
cargo test meeting
```

Expected: all meeting-related tests pass.

- [ ] **Step 2: Run TypeScript type check**

Run:

```powershell
cd openless-all/app
.\node_modules\.bin\tsc.CMD --noEmit
```

Expected: no TypeScript errors.

- [ ] **Step 3: Inspect changed files**

Run:

```powershell
git status --short
git diff --stat
```

Expected:

- Changes are limited to meeting storage/types/IPC files.
- No ASR, recording, UI, or summary-generation behavior was modified.

- [ ] **Step 4: Commit only after user approval**

Do not commit automatically. If the user explicitly asks to commit, use precise staging:

```powershell
git add docs/meeting-recording-v1-storage-plan.md `
  openless-all/app/src-tauri/src/types.rs `
  openless-all/app/src-tauri/src/persistence/mod.rs `
  openless-all/app/src-tauri/src/persistence/paths.rs `
  openless-all/app/src-tauri/src/persistence/meeting.rs `
  openless-all/app/src-tauri/src/commands/mod.rs `
  openless-all/app/src-tauri/src/commands/meetings.rs `
  openless-all/app/src-tauri/src/lib.rs `
  openless-all/app/src/lib/types.ts `
  openless-all/app/src/lib/ipc/index.ts `
  openless-all/app/src/lib/ipc/meetings.ts `
  openless-all/app/src/lib/ipc/mock-data.ts
```

Suggested commit message:

```text
feat(meetings): add meeting storage foundation
```

## Self-Review Checklist

- V1-1 has no UI work.
- V1-1 has no recording or ASR work.
- Meeting audio uses `meeting-recordings/`, not `recordings/`.
- Short-dictation history behavior is untouched.
- Audio retention count is `0..=100`, default 20.
- All new IPC-facing types have Rust and TypeScript mirrors.
- Missing/invalid meeting ids fail with clear errors.
