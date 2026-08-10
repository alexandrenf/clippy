import { useEffect, useRef } from "react";
import { useMotionExit } from "../motion";

interface Props {
  title: string;
  detail: string;
  confirmLabel: string;
  onCancel: () => void;
  onConfirm: () => void;
}

export default function ConfirmDialog({
  title,
  detail,
  confirmLabel,
  onCancel,
  onConfirm,
}: Props) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  const { isExiting, requestExit } = useMotionExit(onCancel);

  useEffect(() => {
    cancelRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        requestExit();
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [requestExit]);

  return (
    <div
      className={`overlay confirm-overlay${isExiting ? " is-closing" : ""}`}
      onMouseDown={requestExit}
    >
      <div
        className="confirm-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="confirm-title"
        aria-describedby="confirm-detail"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <div id="confirm-title" className="confirm-title">
          {title}
        </div>
        <div id="confirm-detail" className="confirm-detail">
          {detail}
        </div>
        <div className="confirm-actions">
          <button ref={cancelRef} className="btn" onClick={requestExit}>
            Cancel
          </button>
          <button className="btn confirm-btn" onClick={onConfirm}>
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
