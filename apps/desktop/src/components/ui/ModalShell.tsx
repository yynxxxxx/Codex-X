import { useEffect, useId, useRef } from "react";
import type { MouseEvent, ReactNode, RefObject } from "react";
import { createPortal } from "react-dom";
import { X } from "lucide-react";

import { IconButton } from "./IconButton";
import { cx } from "./utils";

export interface ModalShellProps {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  children: ReactNode;
  description?: ReactNode;
  footer?: ReactNode;
  ariaLabel?: string;
  size?: "sm" | "md" | "lg" | "xl";
  closeOnBackdrop?: boolean;
  closeOnEscape?: boolean;
  initialFocusRef?: RefObject<HTMLElement>;
  closeLabel?: string;
  showCloseButton?: boolean;
  className?: string;
  bodyClassName?: string;
}

const focusableSelector = [
  "button:not([disabled])",
  "[href]",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[tabindex]:not([tabindex='-1'])",
].join(", ");

let scrollLockCount = 0;
let previousBodyOverflow = "";
let previousBodyPaddingRight = "";

function lockBodyScroll() {
  if (scrollLockCount === 0) {
    previousBodyOverflow = document.body.style.overflow;
    previousBodyPaddingRight = document.body.style.paddingRight;
    const scrollbarGap = window.innerWidth - document.documentElement.clientWidth;
    const bodyPaddingRight = Number.parseFloat(window.getComputedStyle(document.body).paddingRight) || 0;
    document.body.style.overflow = "hidden";
    if (scrollbarGap > 0) document.body.style.paddingRight = `${bodyPaddingRight + scrollbarGap}px`;
  }
  scrollLockCount += 1;

  return () => {
    scrollLockCount = Math.max(0, scrollLockCount - 1);
    if (scrollLockCount === 0) {
      document.body.style.overflow = previousBodyOverflow;
      document.body.style.paddingRight = previousBodyPaddingRight;
    }
  };
}

function focusElement(element: HTMLElement | null) {
  if (!element) return;
  element.focus({ preventScroll: true });
}

export function ModalShell({
  open,
  onClose,
  title,
  children,
  description,
  footer,
  ariaLabel,
  size = "md",
  closeOnBackdrop = true,
  closeOnEscape = true,
  initialFocusRef,
  closeLabel = "关闭",
  showCloseButton = true,
  className,
  bodyClassName,
}: ModalShellProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const descriptionId = useId();

  useEffect(() => {
    if (!open) return;

    const restoreTarget = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const unlockBodyScroll = lockBodyScroll();
    const frame = window.requestAnimationFrame(() => {
      const dialog = dialogRef.current;
      if (!dialog) return;
      focusElement(initialFocusRef?.current || dialog.querySelector<HTMLElement>("[data-initial-focus]") || dialog);
    });

    return () => {
      window.cancelAnimationFrame(frame);
      unlockBodyScroll();
      if (restoreTarget?.isConnected) {
        window.requestAnimationFrame(() => focusElement(restoreTarget));
      }
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;

    const handleKeyDown = (event: KeyboardEvent) => {
      const dialog = dialogRef.current;
      if (!dialog) return;

      if (event.key === "Escape" && closeOnEscape) {
        event.preventDefault();
        event.stopPropagation();
        onClose();
        return;
      }

      if (event.key !== "Tab") return;
      const focusable = Array.from(dialog.querySelectorAll<HTMLElement>(focusableSelector)).filter(
        (element) => element.getClientRects().length > 0,
      );
      if (!focusable.length) {
        event.preventDefault();
        focusElement(dialog);
        return;
      }

      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && (document.activeElement === first || !dialog.contains(document.activeElement))) {
        event.preventDefault();
        focusElement(last);
      } else if (!event.shiftKey && (document.activeElement === last || !dialog.contains(document.activeElement))) {
        event.preventDefault();
        focusElement(first);
      }
    };

    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [closeOnEscape, onClose, open]);

  if (!open) return null;

  const handleBackdropClick = (event: MouseEvent<HTMLDivElement>) => {
    if (closeOnBackdrop && event.target === event.currentTarget) onClose();
  };

  return createPortal(
    <div className="ui-modal-backdrop" onClick={handleBackdropClick} data-modal-backdrop>
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={ariaLabel}
        aria-labelledby={ariaLabel ? undefined : titleId}
        aria-describedby={description ? descriptionId : undefined}
        tabIndex={-1}
        className={cx("ui-modal", `ui-modal--${size}`, className)}
        data-modal-dialog
      >
        <header className="ui-modal__header">
          <div className="ui-modal__heading">
            <h2 id={titleId} className="ui-modal__title">{title}</h2>
            {description && <p id={descriptionId} className="ui-modal__description">{description}</p>}
          </div>
          {showCloseButton && (
            <IconButton
              icon={<X size={17} strokeWidth={2} />}
              label={closeLabel}
              variant="ghost"
              size="sm"
              onClick={onClose}
              className="ui-modal__close"
            />
          )}
        </header>
        <div className={cx("ui-modal__body", bodyClassName)}>{children}</div>
        {footer && <footer className="ui-modal__footer">{footer}</footer>}
      </div>
    </div>,
    document.body,
  );
}
