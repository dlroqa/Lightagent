import { useEffect, useState } from "react";
import { Outlet, useLocation } from "react-router-dom";
import { Menu as MenuIcon } from "lucide-react";

import { agentApi } from "../api/agent";
import { useMediaQuery } from "../hooks/useMediaQuery";
import { usePoll } from "../hooks/usePoll";
import { usePreferences } from "../state/preferences";

/** The standalone frame shared by every Lightagent screen. */
export function Shell() {
  const { preferences } = usePreferences();
  const location = useLocation();
  const mobile = useMediaQuery("(max-width: 760px)");
  const chatRoute = location.pathname === "/" || location.pathname === "/agent";
  const tablet = useMediaQuery("(max-width: 1100px) and (min-width: 761px)");
  const collapsed = mobile ? false : chatRoute || tablet ? true : preferences.railCollapsed;
  const [drawerOpen, setDrawerOpen] = useState(false);
  const tools = usePoll(() => agentApi.tools().then((body) => body.tools), 10_000);

  useEffect(() => setDrawerOpen(false), [location.pathname]);
  useEffect(() => {
    if (!mobile) setDrawerOpen(false);
  }, [mobile]);

  return (
    <div className={`shell shell--workspace${collapsed ? " is-collapsed" : ""}${chatRoute ? " shell--chat" : ""}`}>
      <div className="shell__frame" aria-hidden="true" />

      {mobile && drawerOpen && (
        <button type="button" className="drawer-scrim" aria-label="Close the menu"
          onClick={() => setDrawerOpen(false)} />
      )}

      {!chatRoute && (
      <nav className={`rail${mobile && drawerOpen ? " is-open" : ""}`}
        aria-label="Workspace" aria-hidden={mobile && !drawerOpen}>
        <div className="rail__brand">
          <img className="rail__mark" src="/icon.png" alt="" width={38} height={38} />
          {!collapsed && (
            <span className="rail__name">
              <strong>Lightagent</strong>
              <span>Agent Harness</span>
            </span>
          )}
        </div>

        <div className="rail__spacer" />

        {!collapsed && (
          <div className="railcard">
            <span className="railcard__label">Harness</span>
            <span style={{ display: "flex", alignItems: "center", gap: 7,
              color: tools.error ? "var(--danger)" : "var(--ok)", fontSize: 13, fontWeight: 500 }}>
              <span className="dot" />
              {tools.error ? "Unavailable" : "Ready"}
            </span>
            <span className="railcard__line">
              {tools.data ? `${tools.data.length} runtime tools` : "Loading runtime tools"}
            </span>
            <span className="railcard__line">Provider-neutral agent loop</span>
          </div>
        )}

        <div className="rail__account" aria-label="Lightagent local account">
          <span className="rail__avatar" aria-hidden="true">LA</span>
          {!collapsed && <span className="rail__account-label"><strong>Lightagent</strong><small>Local account</small></span>}
        </div>

      </nav>
      )}
      <main className={`main${chatRoute ? " main--chat" : ""}`}>
        <div className="mobilebar">
          <button type="button" className="btn btn--icon" aria-label="Open the menu"
            aria-expanded={drawerOpen} onClick={() => setDrawerOpen(true)}>
            <MenuIcon size={18} />
          </button>
          <img className="rail__mark" src="/icon.png" alt="" width={26} height={26}
            style={{ width: 26, height: 26 }} />
          <span className="mobilebar__name">Lightagent</span>
        </div>
        <Outlet />
      </main>
    </div>
  );
}

export function TopBar({ title, subtitle, actions }: {
  title: string;
  subtitle: string;
  actions?: React.ReactNode;
}) {
  return (
    <header className="topbar">
      <div className="topbar__titles">
        <h1>{title}</h1>
        <p>{subtitle}</p>
      </div>
      {actions && <div className="topbar__actions">{actions}</div>}
    </header>
  );
}
