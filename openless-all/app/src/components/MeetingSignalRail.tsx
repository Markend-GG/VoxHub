import { useEffect, useRef, useState, type CSSProperties } from 'react';
import {
  MEETING_SIGNAL_MAX_FPS,
  meetingSignalFallbackColor,
  meetingSignalMode,
  meetingSignalModeCode,
  normalizeMeetingSignalLevel,
} from '../lib/meetingCompanionSignal';
import type {
  MeetingCompanionErrorKind,
  MeetingCompanionVisualState,
} from '../lib/meetingCompanionState';

const VERTEX_SHADER = `
attribute vec2 aPosition;
void main() {
  gl_Position = vec4(aPosition, 0.0, 1.0);
}`;

const FRAGMENT_SHADER = `
precision highp float;
uniform vec2 uResolution;
uniform float uTime;
uniform float uLevel;
uniform float uMode;

float hash(float value) {
  return fract(sin(value * 91.17 + 17.31) * 43758.5453);
}

void main() {
  vec2 uv = gl_FragCoord.xy / uResolution;
  float count = 24.0;
  float barId = floor(uv.x * count);
  float cellX = fract(uv.x * count);
  float barMask = smoothstep(0.12, 0.22, cellX) * (1.0 - smoothstep(0.78, 0.88, cellX));
  float centerY = abs(uv.y - 0.5) * 2.0;
  float seed = hash(barId + 1.0);
  float amp = 0.12;
  vec3 color = vec3(0.47, 0.87, 0.57);

  if (uMode < 0.5) {
    amp = 0.12 + 0.05 * sin(uTime * 2.2 + barId * 0.38);
    color = vec3(0.46, 0.78, 0.66);
  } else if (uMode < 1.5) {
    float motion = 0.68 + 0.32 * sin(uTime * (2.4 + seed * 2.2) + barId * 0.71);
    amp = 0.10 + uLevel * motion * (0.64 + seed * 0.28);
    float hot = smoothstep(0.56, 0.92, amp);
    color = mix(vec3(0.35, 0.88, 0.53), vec3(0.98, 0.73, 0.30), hot);
    color = mix(color, vec3(1.0, 0.35, 0.31), smoothstep(0.82, 1.0, amp));
  } else if (uMode < 2.5) {
    amp = 0.18 + uLevel * (0.48 + seed * 0.16);
    color = vec3(0.94, 0.62, 0.25);
  } else if (uMode < 3.5) {
    float distanceToCenter = abs((barId + 0.5) / count - 0.5);
    float gather = 0.12 + 0.38 * (0.5 + 0.5 * sin(uTime * 2.8));
    float focus = 1.0 - smoothstep(gather, gather + 0.16, distanceToCenter);
    amp = 0.10 + focus * (0.34 + 0.20 * sin(uTime * 3.4 + distanceToCenter * 9.0));
    color = mix(vec3(0.29, 0.55, 0.58), vec3(0.55, 0.94, 0.96), focus);
  } else if (uMode < 4.5) {
    float sweep = 1.0 - smoothstep(0.0, 0.16, abs(uv.x - fract(uTime * 0.72)));
    amp = 0.22 + 0.16 * seed + 0.24 * sweep;
    color = mix(vec3(0.32, 0.78, 0.48), vec3(0.72, 1.0, 0.73), sweep);
  } else {
    amp = 0.18 + 0.18 * seed;
    color = vec3(1.0, 0.34, 0.31);
  }

  amp = clamp(amp, 0.08, 0.96);
  float shape = 1.0 - smoothstep(amp - 0.055, amp + 0.015, centerY);
  float baseline = (1.0 - smoothstep(0.055, 0.095, centerY)) * 0.22;
  float alpha = barMask * max(shape, baseline);
  float edgeGlow = (1.0 - smoothstep(0.0, 0.10, abs(centerY - amp))) * barMask * 0.26;
  vec3 outputColor = color * (0.72 + edgeGlow);
  float outputAlpha = clamp(alpha + edgeGlow, 0.0, 1.0);
  gl_FragColor = vec4(outputColor * outputAlpha, outputAlpha);
}`;

interface MeetingSignalRailProps {
  state: MeetingCompanionVisualState;
  level: number;
  errorKind: MeetingCompanionErrorKind | null;
  reducedMotion: boolean;
  frozen?: boolean;
  className?: string;
  style?: CSSProperties;
}

export function MeetingSignalRail({
  state,
  level,
  errorKind,
  reducedMotion,
  frozen = false,
  className,
  style,
}: MeetingSignalRailProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const stateRef = useRef(state);
  const levelRef = useRef(level);
  const errorRef = useRef(errorKind);
  const frozenRef = useRef(frozen);
  const lastLiveLevelRef = useRef(0.18);
  const [webglFailed, setWebglFailed] = useState(false);
  stateRef.current = state;
  levelRef.current = level;
  errorRef.current = errorKind;
  frozenRef.current = frozen;

  useEffect(() => {
    const host = hostRef.current;
    if (!host || reducedMotion) return;
    const canvas = document.createElement('canvas');
    canvas.dataset.meetingSignalCanvas = 'true';
    canvas.style.cssText = 'display:block;width:100%;height:100%;pointer-events:none;';
    host.appendChild(canvas);
    const gl = canvas.getContext('webgl', {
      alpha: true,
      premultipliedAlpha: true,
      antialias: false,
      powerPreference: 'low-power',
    });
    if (!gl) {
      canvas.remove();
      setWebglFailed(true);
      return;
    }

    const compile = (type: number, source: string): WebGLShader | null => {
      const shader = gl.createShader(type);
      if (!shader) return null;
      gl.shaderSource(shader, source);
      gl.compileShader(shader);
      if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
        console.warn('[meeting-signal] shader compile failed', gl.getShaderInfoLog(shader));
        gl.deleteShader(shader);
        return null;
      }
      return shader;
    };

    const vertex = compile(gl.VERTEX_SHADER, VERTEX_SHADER);
    const fragment = compile(gl.FRAGMENT_SHADER, FRAGMENT_SHADER);
    const program = vertex && fragment ? gl.createProgram() : null;
    if (!vertex || !fragment || !program) {
      if (vertex) gl.deleteShader(vertex);
      if (fragment) gl.deleteShader(fragment);
      canvas.remove();
      setWebglFailed(true);
      return;
    }
    gl.attachShader(program, vertex);
    gl.attachShader(program, fragment);
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      console.warn('[meeting-signal] shader link failed', gl.getProgramInfoLog(program));
      gl.deleteProgram(program);
      gl.deleteShader(vertex);
      gl.deleteShader(fragment);
      canvas.remove();
      setWebglFailed(true);
      return;
    }
    setWebglFailed(false);
    gl.useProgram(program);

    const buffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
    const position = gl.getAttribLocation(program, 'aPosition');
    gl.enableVertexAttribArray(position);
    gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);

    const resolutionLocation = gl.getUniformLocation(program, 'uResolution');
    const timeLocation = gl.getUniformLocation(program, 'uTime');
    const levelLocation = gl.getUniformLocation(program, 'uLevel');
    const modeLocation = gl.getUniformLocation(program, 'uMode');
    const frameInterval = 1000 / MEETING_SIGNAL_MAX_FPS;
    let animationFrame = 0;
    let lastFrameAt = 0;
    let lastTickAt = performance.now();
    let animationTime = 0;
    let smoothLevel = 0.18;

    const draw = (now: number) => {
      animationFrame = requestAnimationFrame(draw);
      if (now - lastFrameAt < frameInterval) return;
      const deltaSeconds = Math.min(0.08, Math.max(0, now - lastTickAt) / 1000);
      lastFrameAt = now;
      lastTickAt = now;
      if (!frozenRef.current) animationTime += deltaSeconds;

      const mode = meetingSignalMode(stateRef.current, errorRef.current);
      if (mode === 'live') {
        lastLiveLevelRef.current = normalizeMeetingSignalLevel(levelRef.current);
      }
      const target = mode === 'live'
        ? lastLiveLevelRef.current
        : mode === 'paused'
          ? lastLiveLevelRef.current
          : mode === 'idle'
            ? 0.16
            : 0.28;
      const smoothing = target > smoothLevel ? 15 : 4.5;
      smoothLevel += (target - smoothLevel) * (1 - Math.exp(-deltaSeconds * smoothing));

      const scale = Math.min(window.devicePixelRatio || 1, 1.5) * 0.8;
      const width = Math.max(1, Math.round(canvas.clientWidth * scale));
      const height = Math.max(1, Math.round(canvas.clientHeight * scale));
      if (canvas.width !== width || canvas.height !== height) {
        canvas.width = width;
        canvas.height = height;
        gl.viewport(0, 0, width, height);
      }
      gl.uniform2f(resolutionLocation, width, height);
      gl.uniform1f(timeLocation, animationTime);
      gl.uniform1f(levelLocation, smoothLevel);
      gl.uniform1f(modeLocation, meetingSignalModeCode(mode));
      gl.drawArrays(gl.TRIANGLES, 0, 3);
    };
    draw(performance.now());

    return () => {
      cancelAnimationFrame(animationFrame);
      gl.deleteBuffer(buffer);
      gl.deleteProgram(program);
      gl.deleteShader(vertex);
      gl.deleteShader(fragment);
      gl.getExtension('WEBGL_lose_context')?.loseContext();
      canvas.remove();
    };
  }, [reducedMotion]);

  const mode = meetingSignalMode(state, errorKind);
  const showFallback = reducedMotion || webglFailed;
  return (
    <div
      ref={hostRef}
      className={className}
      data-meeting-signal-mode={mode}
      data-meeting-signal-renderer={showFallback ? 'dom' : 'webgl'}
      aria-hidden="true"
      style={{ position: 'relative', overflow: 'hidden', ...style }}
    >
      {showFallback && (
        <div
          data-meeting-signal-fallback
          style={{
            position: 'absolute',
            inset: 0,
            display: 'flex',
            alignItems: 'center',
            gap: 2,
          }}
        >
          {Array.from({ length: 20 }, (_, index) => {
            const liveLevel = normalizeMeetingSignalLevel(level);
            const base = mode === 'live' ? 5 + liveLevel * (10 + (index % 5) * 2) : 8 + (index % 4) * 2;
            return (
              <span
                key={index}
                style={{
                  width: 2,
                  height: Math.min(26, base),
                  flex: '0 0 2px',
                  borderRadius: 1,
                  background: meetingSignalFallbackColor(mode),
                  opacity: 0.48 + (index % 3) * 0.18,
                }}
              />
            );
          })}
        </div>
      )}
    </div>
  );
}
