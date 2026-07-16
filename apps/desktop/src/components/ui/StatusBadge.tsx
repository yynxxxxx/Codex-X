import type { HTMLAttributes, ReactNode } from "react";

import { cx } from "./utils";

export type StatusBadgeTone = "neutral" | "info" | "success" | "warning" | "danger" | "accent";
export type StatusBadgeSize = "sm" | "md";

export interface StatusBadgeProps extends HTMLAttributes<HTMLSpanElement> {
  tone?: StatusBadgeTone;
  size?: StatusBadgeSize;
  dot?: boolean;
  children: ReactNode;
}

export function StatusBadge({
  tone = "neutral",
  size = "sm",
  dot = true,
  className,
  children,
  ...props
}: StatusBadgeProps) {
  return (
    <span className={cx("ui-status-badge", `ui-status-badge--${tone}`, `ui-status-badge--${size}`, className)} {...props}>
      {dot && <span aria-hidden="true" className="ui-status-badge__dot" />}
      <span>{children}</span>
    </span>
  );
}
