import dayjs from "dayjs";
import relativeTime from "dayjs/plugin/relativeTime";

dayjs.extend(relativeTime);

export function fromNow(input: string | number): string {
  return dayjs(input).fromNow();
}

// Compact age for dense graph rows: "now", "5m", "3h", "2d", "5w", "8mo", "2y".
export function shortAge(input: string | number): string {
  const minutes = dayjs().diff(dayjs(input), "minute");
  if (minutes < 1) return "now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}d`;
  if (days < 60) return `${Math.floor(days / 7)}w`;
  if (days < 365) return `${Math.floor(days / 30)}mo`;
  return `${Math.floor(days / 365)}y`;
}
