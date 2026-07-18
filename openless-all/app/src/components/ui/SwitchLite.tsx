// SwitchLite — small toggle used in the Settings modal sub-sections.

interface SwitchLiteProps {
  on: boolean;
  onToggle: (next: boolean) => void;
  disabled?: boolean;
  ariaLabel: string;
}

export function SwitchLite({ on, onToggle, disabled = false, ariaLabel }: SwitchLiteProps) {
  return (
    <button
      type="button"
      className="ol-focus-ring"
      role="switch"
      aria-checked={on}
      aria-label={ariaLabel}
      disabled={disabled}
      onClick={() => onToggle(!on)}
      style={{
        position: 'relative', width: 32, height: 18, borderRadius: 999, border: 0,
        background: on ? 'var(--ol-blue)' : 'rgba(0,0,0,0.18)',
        cursor: 'default', opacity: disabled ? 0.55 : 1,
        outline: 'none',
      }}
    >
      <span
        style={{
          position: 'absolute', top: 2, left: on ? 16 : 2,
          width: 14, height: 14, borderRadius: 999, background: '#fff',
          boxShadow: '0 1px 2px rgba(0,0,0,.25)', transition: 'left .16s var(--ol-motion-spring)',
        }}
      />
    </button>
  );
}
