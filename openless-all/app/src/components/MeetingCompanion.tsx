import completedPoster from '../assets/meeting-companion/completed-poster.png';
import idlePoster from '../assets/meeting-companion/idle-poster.png';
import pausedPoster from '../assets/meeting-companion/paused-poster.png';
import processingPoster from '../assets/meeting-companion/processing-poster.png';
import quietPoster from '../assets/meeting-companion/quiet-poster.png';
import recordingPoster from '../assets/meeting-companion/recording-poster.png';
import { saveMeetingCompanionPosition, startMeetingCompanionDrag } from '../lib/ipc';
import type { MeetingCompanionVisualState } from '../lib/meetingCompanionState';

const POSTERS: Record<Exclude<MeetingCompanionVisualState, 'hidden'>, string> = {
  idle: idlePoster,
  recording: recordingPoster,
  quiet: quietPoster,
  paused: pausedPoster,
  processing: processingPoster,
  completed: completedPoster,
};

interface MeetingCompanionProps {
  visualState?: MeetingCompanionVisualState;
}

export function MeetingCompanion({ visualState = 'idle' }: MeetingCompanionProps) {
  if (visualState === 'hidden') return null;

  const startDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    void startMeetingCompanionDrag().catch(error => {
      console.warn('[meeting-companion] start drag failed', error);
    });
  };

  const finishDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    void saveMeetingCompanionPosition().catch(error => {
      console.warn('[meeting-companion] save position failed', error);
    });
  };

  return (
    <div
      data-meeting-companion-root
      data-meeting-companion-state={visualState}
      onPointerDown={startDrag}
      onPointerUp={finishDrag}
      onPointerCancel={() => void saveMeetingCompanionPosition()}
      style={{
        width: 350,
        height: 280,
        flex: '0 0 350px',
        position: 'relative',
        overflow: 'hidden',
        cursor: 'grab',
        userSelect: 'none',
        touchAction: 'none',
      }}
    >
      <img
        src={POSTERS[visualState]}
        alt=""
        draggable={false}
        style={{
          display: 'block',
          width: '100%',
          height: '100%',
          objectFit: 'contain',
          pointerEvents: 'none',
        }}
      />
    </div>
  );
}
