import React from "react";
import type { ReactNode } from "react";

const PAGE_TRANSITION_MS = 250;

type PageTransitionPhase = "idle" | "exit" | "enter";

export type PageTransitionProps = {
  pageKey: string;
  children: ReactNode;
};

function prefersReducedMotion() {
  return typeof window !== "undefined"
    && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

export function PageTransition({ pageKey, children }: PageTransitionProps) {
  const [displayedKey, setDisplayedKey] = React.useState(pageKey);
  const [phase, setPhase] = React.useState<PageTransitionPhase>("idle");
  const [reducedMotion, setReducedMotion] = React.useState(prefersReducedMotion);
  const lastDisplayedContent = React.useRef(children);
  const pendingKey = React.useRef(pageKey);

  pendingKey.current = pageKey;

  React.useEffect(() => {
    const media = window.matchMedia("(prefers-reduced-motion: reduce)");
    const handleChange = () => setReducedMotion(media.matches);

    media.addEventListener("change", handleChange);
    return () => media.removeEventListener("change", handleChange);
  }, []);

  React.useLayoutEffect(() => {
    if (displayedKey === pageKey && phase !== "exit") {
      lastDisplayedContent.current = children;
    }
  }, [children, displayedKey, pageKey, phase]);

  React.useEffect(() => {
    if (displayedKey === pageKey || phase === "exit") return;

    if (reducedMotion) {
      setDisplayedKey(pageKey);
      setPhase("idle");
      return;
    }

    setPhase("exit");
  }, [displayedKey, pageKey, phase, reducedMotion]);

  React.useEffect(() => {
    if (phase !== "exit") return;

    if (reducedMotion) {
      setDisplayedKey(pendingKey.current);
      setPhase("idle");
      return;
    }

    const timer = window.setTimeout(() => {
      setDisplayedKey(pendingKey.current);
      setPhase("enter");
    }, PAGE_TRANSITION_MS);

    return () => window.clearTimeout(timer);
  }, [phase, reducedMotion]);

  React.useEffect(() => {
    if (phase !== "enter") return;

    if (reducedMotion) {
      setPhase("idle");
      return;
    }

    const timer = window.setTimeout(() => setPhase("idle"), PAGE_TRANSITION_MS);
    return () => window.clearTimeout(timer);
  }, [phase, reducedMotion]);

  const displayedContent = displayedKey === pageKey
    ? children
    : lastDisplayedContent.current;

  return (
    <div
      className={`cx-page-transition cx-page-transition--${phase}`}
      data-page={displayedKey}
      aria-busy={phase !== "idle"}
    >
      {displayedContent}
    </div>
  );
}
