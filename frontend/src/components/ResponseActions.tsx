import { useCallback, useEffect, useRef, useState, type KeyboardEvent, type ReactNode, type Ref } from "react";
import { Check, Copy, GitBranch, MoreHorizontal, RefreshCw, Share, Square, ThumbsDown, ThumbsUp, Volume2 } from "lucide-react";

import { Menu, MenuItem } from "./Menu";

type Rating = "good" | "bad";

const NOTICE_MS = 2400;

/** The browser's speech synthesis, when it has one. */
const speech = (): SpeechSynthesis | null =>
  typeof window !== "undefined" && "speechSynthesis" in window ? window.speechSynthesis : null;

/**
 * The action row beneath the final assistant answer: copy, rate, read aloud,
 * share, try again, and a "more" menu. It owns only browser-side behaviour;
 * anything that changes the conversation is the caller's, passed as a
 * callback, and an action without one is shown disabled.
 */
export function ResponseActions({
  text,
  timestamp,
  title,
  onRetry,
  onBranch,
}: {
  /** The answer, exactly as it is copied, shared and read aloud. */
  text: string;
  /** When the answer was written, for the "more" menu's header. */
  timestamp: string | null;
  /** The conversation's title, offered to the share sheet. */
  title: string;
  /** Regenerate this answer in place; absent when it cannot be. */
  onRetry?: () => void;
  /** Continue from this answer in a new chat; absent until the API exists. */
  onBranch?: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const [rating, setRating] = useState<Rating | null>(null);
  const [speaking, setSpeaking] = useState(false);
  const [rateOpen, setRateOpen] = useState(false);
  const [moreOpen, setMoreOpen] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const rateButton = useRef<HTMLButtonElement | null>(null);
  const moreButton = useRef<HTMLButtonElement | null>(null);
  const noticeTimer = useRef<number | undefined>(undefined);
  const copiedTimer = useRef<number | undefined>(undefined);
  const utterance = useRef<SpeechSynthesisUtterance | null>(null);
  const canSpeak = speech() !== null;

  const flash = useCallback((message: string) => {
    setNotice(message);
    window.clearTimeout(noticeTimer.current);
    noticeTimer.current = window.setTimeout(() => setNotice(null), NOTICE_MS);
  }, []);

  useEffect(() => () => {
    window.clearTimeout(noticeTimer.current);
    window.clearTimeout(copiedTimer.current);
    // Never leave the browser talking about an answer that is gone.
    if (utterance.current) speech()?.cancel();
  }, []);

  async function copy() {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.clearTimeout(copiedTimer.current);
      copiedTimer.current = window.setTimeout(() => setCopied(false), NOTICE_MS);
      flash("Copied to clipboard");
    } catch {
      flash("Could not copy: the clipboard is unavailable here");
    }
  }

  function rate(next: Rating) {
    // Kept in this view only: the API has no feedback endpoint to send it to.
    setRating((current) => (current === next ? null : next));
    setRateOpen(false);
    flash(rating === next ? "Rating removed" : "Thanks for the feedback");
  }

  function toggleSpeech() {
    const synth = speech();
    if (!synth) {
      flash("Read aloud is not supported in this browser");
      return;
    }
    if (speaking) {
      utterance.current = null;
      synth.cancel();
      setSpeaking(false);
      return;
    }
    synth.cancel();
    const spoken = new SpeechSynthesisUtterance(text);
    const finish = () => {
      // A cancelled or replaced utterance must not reset a newer one.
      if (utterance.current === spoken) {
        utterance.current = null;
        setSpeaking(false);
      }
    };
    spoken.onend = finish;
    spoken.onerror = finish;
    utterance.current = spoken;
    synth.speak(spoken);
    setSpeaking(true);
  }

  async function share() {
    try {
      if (navigator.share) {
        await navigator.share({ title: title || "Lightagent response", text });
        return;
      }
      await navigator.clipboard.writeText(text);
      flash("Sharing is unavailable here, so the response was copied");
    } catch (cause) {
      if (cause instanceof DOMException && cause.name === "AbortError") return;
      flash("Could not share this response");
    }
  }

  /** Arrow keys move between the row's buttons, as in any toolbar. */
  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const keys = ["ArrowLeft", "ArrowRight", "Home", "End"];
    if (!keys.includes(event.key)) return;
    const buttons = [...event.currentTarget.querySelectorAll<HTMLButtonElement>("[data-response-action]")];
    const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (index < 0) return;
    event.preventDefault();
    const last = buttons.length - 1;
    const next = event.key === "Home" ? 0
      : event.key === "End" ? last
        : event.key === "ArrowRight" ? (index === last ? 0 : index + 1)
          : (index === 0 ? last : index - 1);
    buttons[next]?.focus();
  }

  return (
    <div className="response-actions" role="toolbar" aria-label="Response actions" onKeyDown={onKeyDown}>
      <ActionButton label="Copy response" tooltip="Copy response" onClick={() => void copy()}>
        {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
      </ActionButton>
      <ActionButton label="Rate response" tooltip={rateOpen ? undefined : "Rate response"}
        buttonRef={rateButton} expanded={rateOpen} active={rating !== null}
        onClick={() => setRateOpen((open) => !open)}>
        {rating === "good" ? <ThumbsUp aria-hidden="true" className="response-actions__filled" />
          : rating === "bad" ? <ThumbsDown aria-hidden="true" className="response-actions__filled" />
            : <span className="response-actions__rate" aria-hidden="true"><ThumbsUp /><ThumbsDown /></span>}
      </ActionButton>
      <ActionButton label="Read aloud" tooltip="Read aloud" pressed={speaking} active={speaking}
        unavailable={!canSpeak} onClick={toggleSpeech}>
        {speaking ? <Square aria-hidden="true" /> : <Volume2 aria-hidden="true" />}
      </ActionButton>
      <ActionButton label="Share" tooltip="Share" onClick={() => void share()}>
        <Share aria-hidden="true" />
      </ActionButton>
      <ActionButton label="Try again" tooltip="Try again…" disabled={!onRetry} onClick={() => onRetry?.()}>
        <RefreshCw aria-hidden="true" />
      </ActionButton>
      <ActionButton label="More actions" buttonRef={moreButton} expanded={moreOpen}
        onClick={() => setMoreOpen((open) => !open)}>
        <MoreHorizontal aria-hidden="true" />
      </ActionButton>
      <span className={`response-actions__notice${notice ? " is-visible" : ""}`} role="status" aria-live="polite">
        {notice}
      </span>

      <Menu open={rateOpen} anchorRef={rateButton} onClose={() => setRateOpen(false)}
        label="Rate response" placement="above" minWidth={200} className="menu--response">
        <MenuItem checked={rating === "good"} onClick={() => rate("good")}>
          <ThumbsUp size={16} aria-hidden="true" /> Good response
        </MenuItem>
        <MenuItem checked={rating === "bad"} onClick={() => rate("bad")}>
          <ThumbsDown size={16} aria-hidden="true" /> Bad response
        </MenuItem>
      </Menu>
      <Menu open={moreOpen} anchorRef={moreButton} onClose={() => setMoreOpen(false)}
        label="More actions" placement="above" minWidth={240} className="menu--response">
        {timestamp && <div className="menu--response__time">{timestamp}</div>}
        <MenuItem disabled={!onBranch} onClick={() => { setMoreOpen(false); onBranch?.(); }}>
          <GitBranch size={17} aria-hidden="true" />
          <span>Branch in new chat</span>
          {!onBranch && <span className="menu__meta">Not available yet</span>}
        </MenuItem>
      </Menu>
    </div>
  );
}

function ActionButton({
  label,
  tooltip,
  onClick,
  disabled,
  unavailable,
  pressed,
  expanded,
  active,
  buttonRef,
  children,
}: {
  label: string;
  /** Shown beneath the button on hover and keyboard focus. */
  tooltip?: string;
  onClick: () => void;
  disabled?: boolean;
  /** Still focusable and clickable, so it can say why it does nothing. */
  unavailable?: boolean;
  pressed?: boolean;
  /** Set for a button that opens a menu: whether that menu is open. */
  expanded?: boolean;
  active?: boolean;
  buttonRef?: Ref<HTMLButtonElement>;
  children: ReactNode;
}) {
  return (
    <button ref={buttonRef} type="button" data-response-action
      className={`response-actions__button${active || expanded ? " is-active" : ""}`}
      aria-label={label} disabled={disabled} aria-disabled={unavailable || undefined}
      aria-pressed={pressed} aria-haspopup={expanded === undefined ? undefined : "menu"}
      aria-expanded={expanded} onClick={onClick}>
      {children}
      {tooltip && <span className="response-actions__tooltip" aria-hidden="true">{tooltip}</span>}
    </button>
  );
}
