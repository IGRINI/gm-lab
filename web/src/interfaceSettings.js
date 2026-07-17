import { useSyncExternalStore } from "react";

const STORAGE_KEY = "gmlab.interfaceSettings";
const DEFAULTS = Object.freeze({ sceneBackground: false });

function load() {
  if (typeof window === "undefined") return DEFAULTS;
  try {
    const saved = JSON.parse(window.localStorage.getItem(STORAGE_KEY) || "null");
    return {
      sceneBackground: saved?.sceneBackground === true,
    };
  } catch {
    return { ...DEFAULTS };
  }
}

let state = load();
const listeners = new Set();

function persist() {
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    // Private browsing or disabled storage must not break the interface.
  }
}

function setState(next) {
  state = next;
  persist();
  listeners.forEach((listener) => listener());
}

export function setSceneBackground(on) {
  setState({ ...state, sceneBackground: !!on });
}

function subscribe(listener) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function getSnapshot() {
  return state;
}

export function useInterfaceSettings() {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
