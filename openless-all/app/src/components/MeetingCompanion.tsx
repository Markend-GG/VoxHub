import completedPoster from '../assets/meeting-companion/completed-poster.png';
import idlePoster from '../assets/meeting-companion/idle-poster.png';
import pausedPoster from '../assets/meeting-companion/paused-poster.png';
import processingPoster from '../assets/meeting-companion/processing-poster.png';
import quietPoster from '../assets/meeting-companion/quiet-poster.png';
import recordingPoster from '../assets/meeting-companion/recording-poster.png';
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

  return (
    <div
      data-meeting-companion-root
      data-meeting-companion-state={visualState}
      style={{
        width: 350,
        height: 280,
        flex: '0 0 350px',
        position: 'relative',
        overflow: 'hidden',
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
