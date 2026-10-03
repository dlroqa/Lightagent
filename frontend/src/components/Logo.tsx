import type { CSSProperties } from "react";

/** The three supplied Lightagent logo frames, animated by the shared stylesheet. */
export function AnimatedLogo({ className, alt = "", width, height }: {
  className?: string;
  alt?: string;
  width: number;
  height: number;
}) {
  const style = { width, height } satisfies CSSProperties;
  return (
    <span className={`lightagent-logo${className ? ` ${className}` : ""}`} style={style}
      role={alt ? "img" : undefined} aria-label={alt || undefined} aria-hidden={alt ? undefined : true} />
  );
}
