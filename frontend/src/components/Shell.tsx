import { useEffect, useState } from "react";
import { NavLink, Outlet, useLocation } from "react-router-dom";
import {
  Bot,
  ChevronLeft,
  Menu as MenuIcon,
  Moon,
  RotateCw,
  Settings as SettingsIcon,
  Sun,
  Wrench,
} from "lucide-react";

import { agentApi } from "../api/agent";
import { useMediaQuery } from "../hooks/useMediaQuery";
import { usePoll } from "../hooks/usePoll";
import { usePreferences } from "../state/preferences";

const NAV = [
  { to: "/", label: "Agent", icon: Bot, end: true },
  { to: "/tools", label: "Tools", icon: Wrench },
  { to: "/settings", label: "Settings", icon: SettingsIcon },
];

/** The standalone frame shared by every Lightagent screen. */
export function Shell() {
  const { preferences, update } = usePreferences();
  const location = useLocation();
  const mobile = useMediaQuery("(max-width: 760px)");
  const tablet = useMediaQuery("(max-width: 1100px) and (min-width: 761px)");
  const collapsed = mobile ? false : tablet ? true : preferences.railCollapsed;
  const [drawerOpen, setDrawerOpen] = useState(false);
  const tools = usePoll(() => agentApi.tools().then((body) => body.tools), 10_000);

  useEffect(() => setDrawerOpen(false), [location.pathname]);
  useEffect(() => {
    if (!mobile) setDrawerOpen(false);
  }, [mobile]);

  const nextTheme = preferences.theme === "dark" ? "light" : "dark";

  return (
    <div className={`shell${collapsed ? " is-collapsed" : ""}`}>
      <div className="shell__frame" aria-hidden="true" />

      {mobile && drawerOpen && (
        <button type="button" className="drawer-scrim" aria-label="Close the menu"
          onClick={() => setDrawerOpen(false)} />
      )}

      <nav className={`rail${mobile && drawerOpen ? " is-open" : ""}`}
        aria-label="Sections" aria-hidden={mobile && !drawerOpen}>
        <div className="rail__brand">
          <img className="rail__mark" src="/icon.png" alt="" width={38} height={38} />
          {!collapsed && (
            <span className="rail__name">
              <strong>Lightagent</strong>
              <span>Agent Harness</span>
            </span>
          )}
        </div>

        <div className="rail__nav">
          {NAV.map(({ to, label, icon: Icon, end }) => (
            <NavLink key={to} to={to} end={end}
              className={({ isActive }) => `navitem${isActive ? " is-active" : ""}`}
              title={collapsed ? label : undefined}>
              <Icon size={18} strokeWidth={1.9} />
              {!collapsed && <span>{label}</span>}
            </NavLink>
          ))}
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

        <div className="rail__controls">
          <button type="button" className="btn btn--icon"
            title={`Switch to ${nextTheme} theme`} aria-label={`Switch to ${nextTheme} theme`}
            onClick={() => update({ theme: nextTheme })}>
            {preferences.theme === "dark" ? <Sun size={17} /> : <Moon size={17} />}
          </button>
          <button type="button" className="btn btn--icon" title="Refresh"
            aria-label="Refresh the page" onClick={() => window.location.reload()}>
            <RotateCw size={16} />
          </button>
          <button type="button" className="btn btn--icon"
            title={collapsed ? "Expand the sidebar" : "Collapse the sidebar"}
            aria-label={collapsed ? "Expand the sidebar" : "Collapse the sidebar"}
            onClick={() => update({ railCollapsed: !collapsed })}>
            <ChevronLeft size={17}
              style={{ transform: collapsed ? "rotate(180deg)" : undefined }} />
          </button>
        </div>
      </nav>

      <main className="main">
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
