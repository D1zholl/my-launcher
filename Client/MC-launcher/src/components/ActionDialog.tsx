import { useEffect, useRef, useState } from "react";

export interface ActionDialogOptions {
  title: string;
  message: string;
  confirmLabel: string;
  danger?: boolean;
  inputLabel?: string;
  initialValue?: string;
}

export interface ActionDialogRequest extends ActionDialogOptions {
  onConfirm: (value: string) => void;
  onCancel: () => void;
}

interface ActionDialogProps {
  request: ActionDialogRequest | null;
  onClose: () => void;
}

export function ActionDialog({ request, onClose }: ActionDialogProps) {
  const [value, setValue] = useState(request?.initialValue ?? "");
  const dialogRef = useRef<HTMLElement>(null);

  useEffect(() => {
    setValue(request?.initialValue ?? "");
    dialogRef.current?.querySelector<HTMLElement>("input, button")?.focus();
  }, [request]);

  if (!request) return null;
  const activeRequest = request;

  function cancel() {
    activeRequest.onCancel();
    onClose();
  }

  return (
    <div
      className="dialog-backdrop"
      role="presentation"
      onMouseDown={(event) => { if (event.target === event.currentTarget) cancel(); }}
    >
      <section
        ref={dialogRef}
        className="instance-dialog action-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="action-dialog-title"
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            cancel();
          }
          if (event.key !== "Tab") return;
          const focusable = dialogRef.current?.querySelectorAll<HTMLElement>("button:not([disabled]), input:not([disabled])");
          if (!focusable?.length) return;
          const first = focusable[0];
          const last = focusable[focusable.length - 1];
          if (event.shiftKey && document.activeElement === first) {
            event.preventDefault();
            last.focus();
          } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault();
            first.focus();
          }
        }}
      >
        <div className="dialog-heading">
          <div><span className="eyebrow">ПОДТВЕРЖДЕНИЕ</span><h2 id="action-dialog-title">{activeRequest.title}</h2></div>
        </div>
        <form onSubmit={(event) => { event.preventDefault(); activeRequest.onConfirm(value); onClose(); }}>
          <p className="action-dialog-message">{activeRequest.message}</p>
          {activeRequest.inputLabel && (
            <label className="form-field">
              <span>{activeRequest.inputLabel}</span>
              <input autoFocus value={value} onChange={(event) => setValue(event.target.value)} maxLength={64} required />
            </label>
          )}
          <div className="dialog-actions">
            <button className="button-secondary" type="button" onClick={cancel}>Отмена</button>
            <button className={`button-primary${activeRequest.danger ? " button-danger" : ""}`} type="submit">{activeRequest.confirmLabel}</button>
          </div>
        </form>
      </section>
    </div>
  );
}