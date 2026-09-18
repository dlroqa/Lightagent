/** A unix-seconds timestamp as a time for today, or a short date. */
export function whenever(unixSeconds: number): string {
  const at = new Date(unixSeconds * 1000);
  const now = new Date();
  if (at.toDateString() === now.toDateString()) {
    return at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (at.toDateString() === yesterday.toDateString()) return "Yesterday";
  return at.toLocaleDateString([], { month: "short", day: "numeric" });
}
