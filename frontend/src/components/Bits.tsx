import type { ReactNode } from "react";

export function Pill({ tone, children, dot }: {
  tone: "ok" | "warn" | "danger" | "info" | "accent" | "neutral";
  children: ReactNode;
  dot?: boolean;
}) {
  return (
    <span className={`pill pill--${tone}`}>
      {dot && <span className="dot" />}
      {children}
    </span>
  );
}

export function Switch({ checked, onChange, label, disabled }: {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <button type="button" role="switch" className="switch" aria-checked={checked}
      aria-label={label} disabled={disabled} onClick={() => onChange(!checked)} />
  );
}

export function Empty({ title, hint }: { title: string; hint?: string }) {
  return (
    <div className="empty">
      <strong>{title}</strong>
      {hint && <span style={{ maxWidth: "48ch" }}>{hint}</span>}
    </div>
  );
}
