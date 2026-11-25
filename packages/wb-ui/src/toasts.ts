// Ported from the toast part of Fibration Lab apps/desktop/src/state/ui.ts at commit f229e4d.
import { create } from "zustand";

export interface Toast {
  id: number;
  kind: "info" | "error" | "success";
  text: string;
}

interface ToastState {
  toasts: Toast[];
  toast(kind: Toast["kind"], text: string): void;
  dismissToast(id: number): void;
}

let toastId = 0;

export const useToasts = create<ToastState>((set) => ({
  toasts: [],
  toast: (kind, text) => {
    const id = ++toastId;
    set((s) => ({ toasts: [...s.toasts, { id, kind, text }] }));
    setTimeout(() => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })), kind === "error" ? 8000 : 4000);
  },
  dismissToast: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));

export function toast(kind: Toast["kind"], text: string): void {
  useToasts.getState().toast(kind, text);
}

export function notifyError(e: unknown): void {
  const text = e instanceof Error ? e.message : typeof e === "object" && e && "message" in e ? String((e as { message: unknown }).message) : String(e);
  useToasts.getState().toast("error", text);
}
