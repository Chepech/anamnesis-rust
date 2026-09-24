import React from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App.js";
import { installBridge } from "./bridge.js";
import { applyTheme, loadTheme } from "./theme.js";

// Theme before first paint, bridge before any component calls window.anamnesis.
applyTheme(loadTheme());
installBridge();

const container = document.getElementById("root");
if (!container) throw new Error("No #root element");
createRoot(container).render(<App />);
