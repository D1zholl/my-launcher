import { CheckCircle2, CircleAlert, X } from "lucide-react";
import type { ToastNotification } from "../types";

interface ToastProps {
  notification: ToastNotification;
  onDismiss: () => void;
}

export function Toast({ notification, onDismiss }: ToastProps) {
  return (
    <div className="toast-viewport" aria-live="polite">
      <div className={`toast-notice ${notification.kind}`} key={notification.id} role={notification.kind === "error" ? "alert" : "status"}>
        {notification.kind === "error" ? <CircleAlert size={17} /> : <CheckCircle2 size={17} />}
        <span>{notification.message}</span>
        <button type="button" className="toast-close" aria-label="Закрыть уведомление" onClick={onDismiss}><X size={15} /></button>
      </div>
    </div>
  );
}
