import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage } from "../lib/common";
import type { ToastNotification } from "../types";

export function useNotifications() {
  const [notifications, setNotifications] = useState<ToastNotification[]>([]);
  const notification = notifications[0] ?? null;
  const notificationId = useRef(0);

  const showNotification = useCallback(
    (message: string, kind: ToastNotification["kind"] = "error") => {
      notificationId.current += 1;
      setNotifications((current) => [
        ...current.slice(-4),
        { id: notificationId.current, message, kind },
      ]);
    },
    [],
  );

  const reportError = useCallback(
    (error: unknown, fallback?: string) => {
      showNotification(errorMessage(error, fallback), "error");
    },
    [showNotification],
  );

  const dismissNotification = useCallback((id: number) => {
    setNotifications((current) => current.filter((item) => item.id !== id));
  }, []);

  useEffect(() => {
    if (!notification) return;
    const timeout = window.setTimeout(() => {
      setNotifications((current) => current.filter((item) => item.id !== notification.id));
    }, 4200);
    return () => window.clearTimeout(timeout);
  }, [notification?.id]);

  return { notification, showNotification, reportError, dismissNotification };
}
