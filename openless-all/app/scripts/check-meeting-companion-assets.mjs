#!/usr/bin/env node

import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const appRoot = fileURLToPath(new URL('..', import.meta.url));
const assetDir = join(appRoot, 'src', 'assets', 'meeting-companion');
const stateSpecs = [
  { state: 'idle', loop: false, durationMs: 2400 },
  { state: 'recording', loop: true, durationMs: 900 },
  { state: 'quiet', loop: true, durationMs: 2004 },
  { state: 'paused', loop: true, durationMs: 2400 },
  { state: 'processing', loop: true, durationMs: 1200 },
  { state: 'completed', loop: false, durationMs: 2880 },
];
const states = stateSpecs.map((item) => item.state);
const fps = 12;
const durationToleranceMs = Math.ceil(1000 / fps);

function fail(message) {
  console.error(`check-meeting-companion-assets: ${message}`);
  process.exit(1);
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    ...options,
  });
  if (result.error) fail(`${command} is unavailable: ${result.error.message}`);
  if (result.status !== 0) fail(`${command} failed: ${String(result.stderr ?? '').trim()}`);
  return result.stdout;
}

function normalizeTags(tags = {}) {
  return Object.fromEntries(Object.entries(tags).map(([key, value]) => [key.toLowerCase(), value]));
}

function inspectPng(path) {
  const bytes = readFileSync(path);
  if (bytes.subarray(0, 8).toString('hex') !== '89504e470d0a1a0a') fail(`${path} is not a PNG`);
  return {
    width: bytes.readUInt32BE(16),
    height: bytes.readUInt32BE(20),
    bitDepth: bytes[24],
    colorType: bytes[25],
  };
}

function sha256(path) {
  return createHash('sha256').update(readFileSync(path)).digest('hex');
}

const manifest = JSON.parse(readFileSync(join(assetDir, 'manifest.json'), 'utf8'));
if (!Array.isArray(manifest.states) || manifest.states.length !== states.length) {
  fail('manifest must contain exactly six states');
}
if (manifest.states.map((item) => item.state).join(',') !== states.join(',')) {
  fail('manifest states or ordering are invalid');
}
for (const [index, item] of manifest.states.entries()) {
  const expected = stateSpecs[index];
  const keys = Object.keys(item).sort();
  const expectedKeys = ['durationMs', 'loop', 'poster', 'state', 'webm'];
  if (keys.join(',') !== expectedKeys.join(',')) fail(`${item.state}: manifest fields are not minimal`);
  if (item.webm !== `${expected.state}.webm` || item.poster !== `${expected.state}-poster.png`) {
    fail(`${item.state}: manifest media filenames do not match the state`);
  }
  if (item.loop !== expected.loop || item.durationMs !== expected.durationMs) {
    fail(`${item.state}: manifest timing does not match the V1 specification`);
  }
}

const expectedFiles = new Set([
  'manifest.json',
  ...states.flatMap((state) => [`${state}.webm`, `${state}-poster.png`]),
]);
const actualFiles = readdirSync(assetDir).sort();
const extras = actualFiles.filter((file) => !expectedFiles.has(file));
const missing = [...expectedFiles].filter((file) => !actualFiles.includes(file));
if (extras.length || missing.length) {
  fail(`runtime file set mismatch; missing=[${missing}], extras=[${extras}]`);
}

const results = [];
for (const item of manifest.states) {
  const webmPath = join(assetDir, item.webm);
  const posterPath = join(assetDir, item.poster);
  const poster = inspectPng(posterPath);
  if (poster.width !== 350 || poster.height !== 280 || poster.bitDepth !== 8 || poster.colorType !== 6) {
    fail(`${item.state}: poster must be 350x280 8-bit RGBA PNG`);
  }

  const probe = JSON.parse(run('ffprobe', [
    '-v', 'error', '-count_packets', '-show_streams', '-show_format', '-of', 'json', webmPath,
  ]));
  const videos = probe.streams.filter((stream) => stream.codec_type === 'video');
  const audios = probe.streams.filter((stream) => stream.codec_type === 'audio');
  if (videos.length !== 1 || audios.length !== 0) fail(`${item.state}: expected one video stream and no audio`);
  const video = videos[0];
  const videoTags = normalizeTags(video.tags);
  const formatTags = normalizeTags(probe.format.tags);
  const actualDurationMs = Number(probe.format.duration) * 1000;
  const expectedFrameCount = Math.max(1, Math.floor((item.durationMs * fps) / 1000 + 0.5));
  const actualFrameCount = Number(video.nb_read_packets);

  if (video.codec_name !== 'vp9') fail(`${item.state}: codec is not VP9`);
  if (video.width !== 350 || video.height !== 280) fail(`${item.state}: video size is not 350x280`);
  if (video.avg_frame_rate !== '12/1') fail(`${item.state}: frame rate is not 12 FPS`);
  if (videoTags.alpha_mode !== '1') fail(`${item.state}: alpha_mode=1 is missing`);
  if (formatTags.loop !== (item.loop ? '1' : '0')) fail(`${item.state}: loop metadata does not match manifest`);
  if (actualFrameCount !== expectedFrameCount) {
    fail(`${item.state}: encoded frame count is ${actualFrameCount}, expected ${expectedFrameCount}`);
  }
  if (Math.abs(actualDurationMs - item.durationMs) > durationToleranceMs) {
    fail(`${item.state}: duration ${actualDurationMs}ms differs from ${item.durationMs}ms by more than ${durationToleranceMs}ms`);
  }

  const decoded = run(
    'ffmpeg',
    ['-v', 'error', '-c:v', 'libvpx-vp9', '-i', webmPath, '-map', '0:v:0', '-an', '-f', 'rawvideo', '-pix_fmt', 'rgba', 'pipe:1'],
    { encoding: null },
  );
  let alphaMin = 255;
  let alphaMax = 0;
  for (let index = 3; index < decoded.length; index += 4) {
    alphaMin = Math.min(alphaMin, decoded[index]);
    alphaMax = Math.max(alphaMax, decoded[index]);
  }
  if (alphaMin !== 0 || alphaMax !== 255) {
    fail(`${item.state}: decoded alpha range is ${alphaMin}-${alphaMax}, expected 0-255`);
  }

  results.push({
    state: item.state,
    loop: item.loop,
    frames: actualFrameCount,
    configuredMs: item.durationMs,
    actualMs: actualDurationMs,
    alpha: `${alphaMin}-${alphaMax}`,
    webmSha256: sha256(webmPath),
    posterSha256: sha256(posterPath),
  });
}

console.table(results);
console.log('check-meeting-companion-assets: OK');
