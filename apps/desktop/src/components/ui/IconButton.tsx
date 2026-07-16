import type { ButtonHTMLAttributes, ReactNode } from "react";

import { cx } from "./utils";

export type IconButtonVariant = "neutral" | "primary" | "danger" | "ghost";
export type IconButtonSize = "sm" | "md" | "lg";

export interface IconButtonProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children" | "aria-label"> {
  icon: ReactNode;
  label: string;
  variant?: IconButtonVariant;
  size?: IconButtonSize;
}

export function IconButton({
  icon,
  label,
  variant = "neutral",
  size = "md",
  className,
  title,
  ...props
}: IconButtonProps) {
  return (
    <button
      type="button"
      aria-label={label}
      title={title ?? label}
      className={cx("ui-icon-button", `ui-icon-button--${variant}`, `ui-icon-button--${size}`, className)}
      {...props}
    >
      {icon}
    </button>
  );
}
