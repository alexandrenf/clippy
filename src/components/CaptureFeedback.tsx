import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";

type FeedbackKind = "captured" | "empty" | "duplicate" | "error";

interface FeedbackPayload {
  kind: FeedbackKind;
  preview: string;
}

const COPY: Record<FeedbackKind, string> = {
  captured: "Saved to Clippy",
  empty: "Nothing selected",
  duplicate: "Already in Clippy",
  error: "Couldn’t capture",
};

export default function CaptureFeedback() {
  const [feedback, setFeedback] = useState<(FeedbackPayload & { sequence: number }) | null>(null);
  const [exiting, setExiting] = useState(false);
  const [secondsLeft, setSecondsLeft] = useState(2);
  const exitTimer = useRef<number>();
  const countdownTimer = useRef<number>();

  useEffect(() => {
    document.body.classList.add("capture-feedback-surface");
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<FeedbackPayload>("capture-feedback", ({ payload }) => {
      if (disposed) return;
      window.clearTimeout(exitTimer.current);
      window.clearTimeout(countdownTimer.current);
      setExiting(false);
      setSecondsLeft(2);
      setFeedback((current) => ({ ...payload, sequence: (current?.sequence ?? 0) + 1 }));
      countdownTimer.current = window.setTimeout(() => setSecondsLeft(1), 850);
      exitTimer.current = window.setTimeout(() => setExiting(true), 1_650);
    }).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    });
    return () => {
      disposed = true;
      document.body.classList.remove("capture-feedback-surface");
      window.clearTimeout(exitTimer.current);
      window.clearTimeout(countdownTimer.current);
      unlisten?.();
    };
  }, []);

  if (!feedback) return null;
  return (
    <div
      key={feedback.sequence}
      className={`capture-feedback ${exiting ? "exiting" : ""}`}
      data-kind={feedback.kind}
      role="status"
      aria-live="polite"
    >
      <span className="capture-feedback-copy">
        <span className="capture-feedback-heading">
          <strong>{COPY[feedback.kind]}</strong>
          <span key={secondsLeft} className="capture-feedback-countdown" aria-hidden>
            {secondsLeft}s
          </span>
        </span>
        <span>{feedback.preview}</span>
      </span>
    </div>
  );
}
