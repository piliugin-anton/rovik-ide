// localPrefs.ts
//
// A small JSON object in localStorage: this machine's choices for one
// feature (a "don't ask again"). Unreadable or unavailable storage reads
// as no choices, and a save that fails just isn't remembered.

export function loadPrefs<T extends object>(key: string): Partial<T> {
  try {
    return JSON.parse(localStorage.getItem(key) ?? '{}') ?? {};
  } catch {
    return {};
  }
}

export function savePrefs<T extends object>(key: string, patch: Partial<T>) {
  try {
    localStorage.setItem(key, JSON.stringify({ ...loadPrefs<T>(key), ...patch }));
  } catch {
    // storage unavailable: the choice just isn't remembered
  }
}
