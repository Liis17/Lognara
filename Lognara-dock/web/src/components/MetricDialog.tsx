import { useEffect, useId, useRef, type ReactNode } from "react";

export default function MetricDialog({
  title,
  onClose,
  children,
  busy = false,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  busy?: boolean;
}) {
  const titleId = useId();
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    const dialog = ref.current!;
    dialog.showModal();
    return () => {
      dialog.close();
      previous?.focus();
    };
  }, []);
  return (
    <dialog
      ref={ref}
      className="metric-dialog"
      aria-labelledby={titleId}
      onCancel={(e) => {
        e.preventDefault();
        if (!busy) onClose();
      }}
    >
      <div className="toolbar">
        <h2 id={titleId}>{title}</h2>
        <button onClick={onClose} disabled={busy} aria-label="Закрыть">
          ×
        </button>
      </div>
      {children}
    </dialog>
  );
}
