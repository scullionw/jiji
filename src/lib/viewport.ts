/** Prefix offsets let fixed-height content be windowed without measuring rows. */
export function rowOffsets<T>(rows: readonly T[], height: (row: T) => number): number[] {
  const offsets = [0];
  for (const row of rows) offsets.push(offsets[offsets.length - 1] + height(row));
  return offsets;
}

/** Half-open range of rows intersecting the viewport, including overscan. */
export function visibleRange(
  offsets: readonly number[],
  top: number,
  height: number,
  overscan = 0,
): { start: number; end: number } {
  const count = Math.max(0, offsets.length - 1);
  const lower = top - overscan;
  const upper = top + height + overscan;
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

type Viewport = { top: number; bottom: number; height: number };
type Listener = (viewport: Viewport) => void;
const observers = new WeakMap<HTMLElement, { listeners: Set<Listener>; stop: () => void }>();

/** One scroll listener, resize observer and viewport read per scrollport per frame. */
export function observeViewport(scroller: HTMLElement, listener: Listener): () => void {
  let observer = observers.get(scroller);
  if (!observer) {
    const listeners = new Set<Listener>();
    let frame = 0;
    const publish = () => {
      frame = 0;
      const { top } = scroller.getBoundingClientRect();
      const height = scroller.clientHeight;
      const viewport = { top, bottom: top + height, height };
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
