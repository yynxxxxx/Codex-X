import type { InputHTMLAttributes, ReactNode } from "react";

import { cx } from "./utils";

export interface ToggleProps
  extends Omit<InputHTMLAttributes<HTMLInputElement>, "type" | "checked" | "defaultChecked" | "onChange" | "className" | "role"> {
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  label?: ReactNode;
  description?: ReactNode;
  className?: string;
  inputClassName?: string;
}

export function Toggle({
  checked,
  onCheckedChange,
  label,
  description,
  className,
  inputClassName,
  disabled,
  ...props
}: ToggleProps) {
  return (
    <label className={cx("ui-toggle", disabled && "ui-toggle--disabled", className)}>
      <input
        {...props}
        type="checkbox"
        role="switch"
        checked={checked}
        disabled={disabled}
        aria-checked={checked}
        className={cx("ui-toggle__input", inputClassName)}
        onChange={(event) => onCheckedChange(event.target.checked)}
      />
      <span aria-hidden="true" className="ui-toggle__track" />
      {(label || description) && (
        <span className="ui-toggle__content">
          {label && <span className="ui-toggle__label">{label}</span>}
          {description && <span className="ui-toggle__description">{description}</span>}
        </span>
      )}
    </label>
  );
}
