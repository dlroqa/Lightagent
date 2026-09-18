import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";

/** Browser-local presentation preferences for the standalone Web UI. */
export const DEFAULT_PREFERENCES = {
  theme: "system" as "light" | "dark" | "system",
  translucent: true,
  compact: false,
  railCollapsed: false,
};

export type Preferences = typeof DEFAULT_PREFERENCES;

interface PreferencesValue {
  preferences: Preferences;
  update: (patch: Partial<Preferences>) => void;
}

const MIRROR_KEY = "lightagent.web.preferences";
const PreferencesContext = createContext<PreferencesValue | null>(null);

function readMirror(): Preferences {
  try {
    const raw = window.localStorage.getItem(MIRROR_KEY);
    if (!raw) return DEFAULT_PREFERENCES;
    return { ...DEFAULT_PREFERENCES, ...(JSON.parse(raw) as Partial<Preferences>) };
  } catch {
    return DEFAULT_PREFERENCES;
  }
}

function writeMirror(preferences: Preferences) {
  try {
    window.localStorage.setItem(MIRROR_KEY, JSON.stringify(preferences));
  } catch {
    // Storage is optional; the current page still keeps the preference.
  }
}

export function PreferencesProvider({ children }: { children: ReactNode }) {
  const [preferences, setPreferences] = useState<Preferences>(readMirror);

  useEffect(() => {
    const root = document.documentElement;
    if (preferences.theme === "system") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", preferences.theme);
    root.setAttribute("data-surfaces", preferences.translucent ? "glass" : "solid");
    root.setAttribute("data-density", preferences.compact ? "compact" : "comfortable");
  }, [preferences]);

  const update = useCallback((patch: Partial<Preferences>) => {
    setPreferences((current) => {
      const next = { ...current, ...patch };
      writeMirror(next);
      return next;
    });
  }, []);

  const value = useMemo(() => ({ preferences, update }), [preferences, update]);
  return <PreferencesContext.Provider value={value}>{children}</PreferencesContext.Provider>;
}

export function usePreferences(): PreferencesValue {
  const value = useContext(PreferencesContext);
  if (!value) throw new Error("usePreferences must be used inside PreferencesProvider");
  return value;
}
