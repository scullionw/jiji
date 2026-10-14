/** Prefix offsets let fixed-height content be windowed without measuring rows. */
export function rowOffsets<T>(rows: readonly T[], height: (row: T) => number): number[] {
  const offsets = [0];
  for (const row of rows) offsets.push(offsets[offsets.length - 1] + height(row));
  return offsets;
}

/** Extra distance to keep mounted on each side of the viewport. */
export interface Overscan {
  before: number;
  after: number;
}

/**
 * Overscan that leads the scroll. Async scrolling means the compositor
 * shows new rows a frame or two before the main thread hears about the
 * scroll, and further still while a frame runs long — so the side the
 * content is heading to gets `base` plus the distance it travels in
 * `leadMs` at the current speed (capped at `base + max`), while the
 * trailing side keeps just `base`.
 */
export function leadingOverscan(
  velocity: number,
  base: number,
  leadMs = 100,
  max = Infinity,
): Overscan {
  const lead = Math.min(max, Math.abs(velocity) * leadMs);
  return velocity >= 0
    ? { before: base, after: base + lead }
    : { before: base + lead, after: base };
}

/** Half-open range of rows intersecting the viewport, including overscan. */
export function visibleRange(
  offsets: readonly number[],
  top: number,
  height: number,
  overscan: number | Overscan = 0,
): { start: number; end: number } {
  const before = typeof overscan === "number" ? overscan : overscan.before;
  const after = typeof overscan === "number" ? overscan : overscan.after;
  const count = Math.max(0, offsets.length - 1);
  const lower = top - before;
  const upper = top + height + after;
  if (count === 0 || upper <= 0) return { start: 0, end: 0 };
  if (lower >= offsets[count]) return { start: count, end: count };

  let lo = 0;
  let hi = count;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (offsets[mid + 1] <= lower) lo = mid + 1;
    else hi = mid;
  }
  const start = lo;
  hi = count;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (offsets[mid] < upper) lo = mid + 1;
    else hi = mid;
  }
  return { start, end: lo };
}

/**
 * The scrollport's position on screen, plus how fast its content is
 * moving: `velocity` is scrollTop change in px per ms over the last
 * frame, positive when scrolling down, 0 when still or after a pause.
 */
export interface Viewport {
  top: number;
  bottom: number;
  height: number;
  velocity: number;
}
type Listener = (viewport: Viewport) => void;
const observers = new WeakMap<HTMLElement, { listeners: Set<Listener>; stop: () => void }>();

/// A gap longer than this between two publishes means the scroll had
/// stopped; the next velocity reading starts from rest.
const STALE_MS = 250;

/** One scroll listener, resize observer and viewport read per scrollport per frame. */
export function observeViewport(scroller: HTMLElement, listener: Listener): () => void {
  let observer = observers.get(scroller);
  if (!observer) {
    const listeners = new Set<Listener>();
    let frame = 0;
    let lastTop = scroller.scrollTop;
    let lastTime = 0;
    const publish = () => {
      frame = 0;
      const now = performance.now();
      const scrollTop = scroller.scrollTop;
      const dt = now - lastTime;
      const velocity = lastTime && dt > 0 && dt < STALE_MS ? (scrollTop - lastTop) / dt : 0;
      lastTop = scrollTop;
      lastTime = now;
      const { top } = scroller.getBoundingClientRect();
      const height = scroller.clientHeight;
      const viewport = { top, bottom: top + height, height, velocity };
      for (const notify of listeners) notify(viewport);
    };
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(publish);
    };
    const resize = new ResizeObserver(schedule);
    resize.observe(scroller);
    scroller.addEventListener("scroll", schedule, { passive: true });
    observer = {
      listeners,
      stop() {
        cancelAnimationFrame(frame);
        resize.disconnect();
        scroller.removeEventListener("scroll", schedule);
        observers.delete(scroller);
      },
    };
    observers.set(scroller, observer);
    schedule();
  }
  observer.listeners.add(listener);
  const current = observer;
  return () => {
    current.listeners.delete(listener);
    if (current.listeners.size === 0) current.stop();
  };
}
