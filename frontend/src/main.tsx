import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { HashRouter } from "react-router-dom";

import { App } from "./App";
import { PreferencesProvider } from "./state/preferences";
import "./styles/app.css";

/**
 * `HashRouter` rather than `BrowserRouter`.
 *
 * Lightagent serves `index.html` for any path that has no extension, but hash
 * routing also works when the packaged Web UI is opened from a static host or
 * inspected directly from disk.
 */
const container = document.getElementById("root");
if (!container) {
  throw new Error("the document has no #root to mount into");
}

createRoot(container).render(
  <StrictMode>
    <PreferencesProvider>
      <HashRouter>
        <App />
      </HashRouter>
    </PreferencesProvider>
  </StrictMode>,
);
