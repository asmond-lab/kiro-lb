import { useSyncExternalStore } from "react";

export type AlertTone = "error" | "warning" | "success";

export type AlertInput = {
  tone: AlertTone;
  text?: string;
  key?: string;
  vars?: Record<string, string | number>;
  error?: string;
};

export type AlertItem = AlertInput & { id: number; leaving: boolean };

export const ALERT_FADE_MS = 500;
const VISIBLE_MS: Record<AlertTone, number> = { error: 8000, warning: 7000, success: 4000 };

let items: AlertItem[] = [];
let nextId = 1;
const timers = new Map<number, number[]>();
const listeners = new Set<() => void>();

function emit() {
  listeners.forEach((listener) => listener());
}

function clearTimers(id: number) {
  (timers.get(id) ?? []).forEach((timer) => window.clearTimeout(timer));
  timers.delete(id);
}

function schedule(id: number, tone: AlertTone) {
  clearTimers(id);
  const leave = window.setTimeout(() => dismissAlert(id), VISIBLE_MS[tone]);
  timers.set(id, [leave]);
}

function sameMessage(a: AlertInput, b: AlertInput) {
  return (
    a.tone === b.tone &&
    a.text === b.text &&
    a.key === b.key &&
    a.error === b.error &&
    JSON.stringify(a.vars) === JSON.stringify(b.vars)
  );
}

export function pushAlert(input: AlertInput): number {
  const existing = items.find((item) => !item.leaving && sameMessage(item, input));
  if (existing) {
    schedule(existing.id, input.tone);
    return existing.id;
  }
  const id = nextId++;
  items = [...items, { ...input, id, leaving: false }];
  schedule(id, input.tone);
  emit();
  return id;
}

export function pushError(error: unknown) {
  return pushAlert({ tone: "error", error: error instanceof Error ? error.message : String(error ?? "") });
}

export function dismissAlert(id: number) {
  const item = items.find((entry) => entry.id === id);
  if (!item || item.leaving) return;
  clearTimers(id);
  items = items.map((entry) => (entry.id === id ? { ...entry, leaving: true } : entry));
  emit();
  const remove = window.setTimeout(() => {
    timers.delete(id);
    items = items.filter((entry) => entry.id !== id);
    emit();
  }, ALERT_FADE_MS);
  timers.set(id, [remove]);
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export const getSnapshot = () => items;
const getServerSnapshot = (): AlertItem[] => [];

export function useAlerts(): AlertItem[] {
  return useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);
}
