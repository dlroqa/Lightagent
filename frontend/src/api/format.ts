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

/** When an answer was written, as the response menu heads it: "Today, 3:57 PM". */
export function answerTimestamp(unixSeconds: number): string {
  const at = new Date(unixSeconds * 1000);
  const now = new Date();
  const time = at.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  if (at.toDateString() === now.toDateString()) return `Today, ${time}`;
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (at.toDateString() === yesterday.toDateString()) return `Yesterday, ${time}`;
  return `${at.toLocaleDateString([], { month: "short", day: "numeric" })}, ${time}`;
}

/** A centered transcript marker for a user prompt. */
export function promptTimestamp(unixSeconds: number): string {
  const at = new Date(unixSeconds * 1000);
  const now = new Date();
  const time = at.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  if (at.toDateString() === now.toDateString()) return `Today ${time}`;
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (at.toDateString() === yesterday.toDateString()) return `Yesterday ${time}`;
  return `${at.toLocaleDateString([], { month: "short", day: "numeric" })} ${time}`;
}
