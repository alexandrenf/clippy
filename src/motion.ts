import { useCallback, useEffect, useRef, useState } from "react";

const EXIT_DURATION_MS = 150;

export function useMotionExit(onExited: () => void) {
  const [isExiting, setIsExiting] = useState(false);
  const exitingRef = useRef(false);
  const timerRef = useRef<number>();
  const onExitedRef = useRef(onExited);
  onExitedRef.current = onExited;

  useEffect(
    () => () => {
      window.clearTimeout(timerRef.current);
    },
    [],
  );

  const requestExit = useCallback(() => {
    if (exitingRef.current) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      onExitedRef.current();
      return;
    }

    exitingRef.current = true;
    setIsExiting(true);
    timerRef.current = window.setTimeout(() => onExitedRef.current(), EXIT_DURATION_MS);
  }, []);

  return { isExiting, requestExit };
}
