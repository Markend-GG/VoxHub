import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const source = fs.readFileSync(
  path.join(root, 'src', 'components', 'MeetingCompanion.tsx'),
  'utf8',
);

const requirements = [
  ['native keyboard buttons', (source.match(/<button/g) ?? []).length >= 4],
  ['control tooltip', source.includes('title={label}')],
  ['control aria label', source.includes('aria-label={label}')],
  ['keyboard focus ring', source.includes('className="ol-focus-ring"')],
  ['modal dialog semantics', source.includes('role="dialog"') && source.includes('aria-modal="true"')],
  ['focus trap', source.includes('nextMeetingCompanionDialogFocusIndex(')],
  ['escape cancellation', source.includes("event.key === 'Escape'")],
  ['hide never aliases stop', source.includes("if (action === 'hide')") && source.includes('void interaction.hide();')],
  ['summary failure open entry', source.includes("action === 'open-meeting'") && source.includes('void interaction.openMeeting();')],
  ['native show restores hidden state', source.includes("'meeting-companion:show'") && source.includes("dispatch({ type: 'show'")],
  ['no native confirm', !source.includes('window.confirm(') && !source.includes('globalThis.confirm(')],
];

for (const [name, passed] of requirements) {
  if (!passed) throw new Error(`meeting companion controls contract failed: ${name}`);
}

console.log('meeting companion controls contract: OK');
