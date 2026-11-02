// What the perf budgets of §10 are measured by. On with `?bench`; the Playwright run reads
// it from `window.__ognivoPerf`.
//
// - frames: the interval of every frame;
// - keys: for every key, the time from its timestamp to the start of the frame that shows
//   what it did. That is the app's part of input latency; the compositor adds its own;
// - slow: input events of 16 ms or more to their paint as Event Timing sees them, the
//   compositor included and rounded to 8 ms (it reports nothing faster).

export interface Probe {
  frames: number[];
  keys: number[];
  slow: { name: string; duration: number; handled: number }[];
  /** A stream is playing. */
  streaming: boolean;
}

declare global {
  interface Window {
    __ognivoPerf?: Probe;
  }
}

export function startProbe(): Probe {
  const probe: Probe = { frames: [], keys: [], slow: [], streaming: false };
  window.__ognivoPerf = probe;
  let last = 0;
  const frame = (t: number) => {
    if (last) probe.frames.push(t - last);
    last = t;
    requestAnimationFrame(frame);
  };
  requestAnimationFrame(frame);
  // React applies a key's change before the next frame; that frame starts at this callback
  window.addEventListener(
    "keydown",
    (e) => {
      const at = e.timeStamp;
      requestAnimationFrame(() => probe.keys.push(performance.now() - at));
    },
    { capture: true },
  );
  new PerformanceObserver((list) => {
    for (const e of list.getEntries() as PerformanceEventTiming[]) {
      probe.slow.push({ name: e.name, duration: e.duration, handled: e.processingEnd - e.startTime });
    }
  }).observe({ type: "event", durationThreshold: 16, buffered: true } as PerformanceObserverInit);
  return probe;
}
