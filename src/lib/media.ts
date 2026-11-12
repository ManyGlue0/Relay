import { api, fileUrl, type Attachment } from "./api";

/** Speech models listen at 16 kHz; mono WAV at that rate is what every server takes. */
const RATE = 16000;
/** Longest sound Relay converts (a 10-minute WAV is about 19 MB). */
const MAX_SOUND_SECONDS = 20 * 60;

/** Mono 16-bit PCM WAV at `RATE`, from decoded audio. */
async function wavOf(audio: AudioBuffer, maxSeconds = MAX_SOUND_SECONDS): Promise<Uint8Array> {
  const seconds = Math.min(audio.duration, maxSeconds);
  const frames = Math.max(1, Math.ceil(seconds * RATE));
  const offline = new OfflineAudioContext(1, frames, RATE);
  const src = offline.createBufferSource();
  src.buffer = audio;
  src.connect(offline.destination);
  src.start();
  const mono = (await offline.startRendering()).getChannelData(0);
  return encodeWav(mono, RATE);
}

function encodeWav(samples: Float32Array, rate: number): Uint8Array {
  const out = new ArrayBuffer(44 + samples.length * 2);
  const v = new DataView(out);
  const str = (o: number, s: string) => [...s].forEach((c, i) => v.setUint8(o + i, c.charCodeAt(0)));
  str(0, "RIFF");
  v.setUint32(4, 36 + samples.length * 2, true);
  str(8, "WAVE");
  str(12, "fmt ");
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true); // PCM
  v.setUint16(22, 1, true); // mono
  v.setUint32(24, rate, true);
  v.setUint32(28, rate * 2, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  str(36, "data");
  v.setUint32(40, samples.length * 2, true);
  for (let i = 0; i < samples.length; i++) {
    const s = Math.max(-1, Math.min(1, samples[i]));
    v.setInt16(44 + i * 2, s < 0 ? s * 0x8000 : s * 0x7fff, true);
  }
  return new Uint8Array(out);
}

async function decode(bytes: ArrayBuffer): Promise<AudioBuffer> {
  const ctx = new AudioContext();
  try {
    return await ctx.decodeAudioData(bytes);
  } finally {
    void ctx.close();
  }
}

/** Loads a video for reading frames (CORS, so the canvas isn't tainted). */
function loadVideo(url: string): Promise<HTMLVideoElement> {
  return new Promise((resolve, reject) => {
    const v = document.createElement("video");
    v.crossOrigin = "anonymous";
    v.muted = true;
    v.preload = "auto";
    v.onloadeddata = () => resolve(v);
    v.onerror = () => reject(new Error("The video couldn't be read"));
    v.src = url;
  });
}

function seek(v: HTMLVideoElement, t: number): Promise<void> {
  return new Promise((resolve) => {
    const done = () => {
      v.removeEventListener("seeked", done);
      resolve();
    };
    v.addEventListener("seeked", done);
    v.currentTime = t;
  });
}

/** One still every few seconds: at least 4, at most 10 (each costs the model an image). */
function stillTimes(duration: number): number[] {
  const n = Math.max(4, Math.min(10, Math.round(duration / 3)));
  if (!isFinite(duration) || duration <= 0) return [0];
  // Mid-points of n equal slices: no black first frame, no fade-out last one.
  return Array.from({ length: n }, (_, i) => Math.round(((i + 0.5) * duration) / n * 10) / 10);
}

/**
 * Prepares what the model may need from a file: a video's stills, length,
 * size and sound; an audio file's length and (unless it's WAV or MP3) a WAV
 * copy. Problems leave the file as it is; it can still be sent.
 */
export async function prepare(a: Attachment): Promise<Attachment> {
  let out = a;
  try {
    if (a.kind === "video") {
      const v = await loadVideo(fileUrl(a));
      const longest = 768;
      const scale = Math.min(1, longest / Math.max(v.videoWidth, v.videoHeight, 1));
      const canvas = document.createElement("canvas");
      canvas.width = Math.max(1, Math.round(v.videoWidth * scale));
      canvas.height = Math.max(1, Math.round(v.videoHeight * scale));
      const g = canvas.getContext("2d")!;
      out = await api.attachInfo(a.id, { duration: v.duration, width: v.videoWidth, height: v.videoHeight });
      for (const [i, t] of stillTimes(v.duration).entries()) {
        await seek(v, Math.min(t, Math.max(0, v.duration - 0.05)));
        g.drawImage(v, 0, 0, canvas.width, canvas.height);
        const blob = await new Promise<Blob | null>((r) => canvas.toBlob(r, "image/jpeg", 0.82));
        if (!blob) break;
        out = await api.attachPut(a.id, `frame-${i}.jpg`, new Uint8Array(await blob.arrayBuffer()), t);
      }
      v.removeAttribute("src");
      v.load();
      // Its sound, for models that hear (a video without sound fails to decode: fine).
      try {
        const bytes = await (await fetch(fileUrl(a))).arrayBuffer();
        out = await api.attachPut(a.id, "sound.wav", await wavOf(await decode(bytes)));
      } catch {
        /* no sound track */
      }
    } else if (a.kind === "audio" && !a.voice) {
      const bytes = await (await fetch(fileUrl(a))).arrayBuffer();
      const audio = await decode(bytes);
      out = await api.attachInfo(a.id, { duration: audio.duration });
      if (!/\.(wav|mp3)$/i.test(a.file)) out = await api.attachPut(a.id, "sound.wav", await wavOf(audio));
    }
  } catch {
    /* the webview can't decode it; the file still goes as it is */
  }
  return out;
}

// ---- recording ---------------------------------------------------------------------

export interface Recording {
  /** 0..1, how loud right now (for the level meter). */
  level(): number;
  /** Stops and returns the recording as WAV, with its length in seconds. */
  stop(): Promise<{ wav: Uint8Array; seconds: number }>;
  cancel(): void;
}

/** Starts recording from the microphone. */
export async function record(): Promise<Recording> {
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true },
  });
  const ctx = new AudioContext();
  const analyser = ctx.createAnalyser();
  analyser.fftSize = 512;
  ctx.createMediaStreamSource(stream).connect(analyser);
  const samples = new Float32Array(analyser.fftSize);
  const recorder = new MediaRecorder(stream, MediaRecorder.isTypeSupported("audio/webm;codecs=opus") ? { mimeType: "audio/webm;codecs=opus" } : undefined);
  const chunks: Blob[] = [];
  recorder.ondataavailable = (e) => e.data.size && chunks.push(e.data);
  recorder.start(250);
  const release = () => {
    stream.getTracks().forEach((t) => t.stop());
    void ctx.close();
  };
  return {
    level() {
      analyser.getFloatTimeDomainData(samples);
      let sum = 0;
      for (const s of samples) sum += s * s;
      // RMS, eased so quiet speech still moves the meter.
      return Math.min(1, Math.sqrt(Math.sqrt(sum / samples.length)) * 1.6);
    },
    stop() {
      return new Promise((resolve, reject) => {
        if (recorder.state === "inactive") {
          release();
          return reject(new Error("The recording had already stopped"));
        }
        recorder.onstop = async () => {
          release();
          try {
            const audio = await decode(await new Blob(chunks, { type: recorder.mimeType }).arrayBuffer());
            resolve({ wav: await wavOf(audio), seconds: audio.duration });
          } catch {
            reject(new Error("The recording couldn't be read"));
          }
        };
        recorder.stop();
      });
    },
    cancel() {
      recorder.onstop = null;
      if (recorder.state !== "inactive") recorder.stop();
      release();
    },
  };
}

/** Why the microphone didn't start, in words. */
export function micError(e: unknown): string {
  const name = e instanceof DOMException ? e.name : "";
  if (name === "NotAllowedError" || name === "SecurityError")
    return "Relay isn't allowed to use the microphone. Check Settings → Privacy & security → Microphone in Windows.";
  if (name === "NotFoundError" || name === "OverconstrainedError") return "No microphone found";
  if (name === "NotReadableError") return "The microphone is busy in another app";
  return e instanceof Error ? e.message : "The microphone didn't start";
}

/** "1:05" from seconds. */
export function clock(seconds: number | undefined): string {
  const s = Math.max(0, Math.round(seconds ?? 0));
  return s >= 3600 ? `${Math.floor(s / 3600)}:${String(Math.floor(s / 60) % 60).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}` : `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** "2.4 MB" */
export function fileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(bytes < 10 * 1024 * 1024 ? 1 : 0)} MB`;
}
