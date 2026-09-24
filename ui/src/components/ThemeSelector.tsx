import React, { useState } from "react";
import { THEMES, applyTheme, loadTheme } from "../theme.js";

/** Applies immediately and persists per machine; not part of config.json, so no Save needed. */
export function ThemeSelector(): React.ReactElement {
  const [theme, setTheme] = useState(loadTheme);
  return (
    <select className="select-input" aria-label="Theme" value={theme} onChange={(e) => setTheme(applyTheme(e.target.value))}>
      {THEMES.map((t) => <option key={t.id} value={t.id}>{t.label}</option>)}
    </select>
  );
}
