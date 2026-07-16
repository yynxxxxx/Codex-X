import { useEffect, useRef } from "react";
import type { InputHTMLAttributes, ReactNode } from "react";

import { Check, Minus } from "lucide-react";

import { cx } from "./utils";

export interface CheckboxProps
  extends Omit<InputHTMLAttributes<HTMLInputElement>, "type" | "checked" | "defaultChecked" | "onChange" | "className"> {
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  label?: ReactNode;
  description?: ReactNode;
  indeterminate?: boolean;
  className?: string;
  inputClassName?: string;
}

export function Checkbox({
  checked,
  onCheckedChange,
  label,
  description,
  indeterminate = false,
  className,
  inputClassName,
  disabled,
  ...props
}: CheckboxProps) {
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (inputRef.current) inputRef.current.indeterminate = indeterminate;
  }, [indeterminate]);

  return (
    <label className={cx("ui-checkbox", disabled && "ui-checkbox--disabled", className)}>
      <input
        {...props}
        ref={inputRef}
        type="checkbox"
        checked={checked}
        disabled={disabled}
        aria-checked={indeterminate ? "mixed" : checked}
        className={cx("ui-checkbox__input", inputClassName)}
        onChange={(event) => onCheckedChange(event.target.checked)}
      />
      <span
        aria-hidden="true"
        className={cx("ui-checkbox__box", checked && "ui-checkbox__box--checked", indeterminate && "ui-checkbox__box--indeterminate")}
      >
        {indeterminate ? <Minus size={13} strokeWidth={3} /> : <Check size={13} strokeWidth={3} />}
      </span>
      {(label || description) && (
        <span className="ui-checkbox__content">
          {label && <span className="ui-checkbox__label">{label}</span>}
          {description && <span className="ui-checkbox__description">{description}</span>}
        </span>
      )}
    </label>
  );
}
