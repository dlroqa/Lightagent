import { useEffect, useRef, useState } from "react";
import { Outlet, useLocation, useNavigate } from "react-router-dom";
import { ArrowLeft, Menu as MenuIcon, PanelLeftClose, PanelLeftOpen, Search, Settings, Wrench } from "lucide-react";

import { useMediaQuery } from "../hooks/useMediaQuery";
import { usePreferences } from "../state/preferences";
import { AnimatedLogo } from "./Logo";
import { Menu, MenuItem } from "./Menu";

/** The standalone frame shared by every Lightagent screen. */
export function Shell() {
  const { preferences, update } = usePreferences();
  const location = useLocation();
  const navigate = useNavigate();
  const mobile = useMediaQuery("(max-width: 760px)");
  const chatRoute = location.pathname === "/" || location.pathname === "/agent";
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [accountOpen, setAccountOpen] = useState(false);
  const accountButton = useRef<HTMLButtonElement | null>(null);
  const collapsed = mobile ? false : chatRoute ? true : preferences.railCollapsed;

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
          {collapsed ? (
            <button type="button" className="rail__collapsed-logo" aria-label="Expand sidebar" title="Expand sidebar"
              onClick={() => update({ railCollapsed: false })}>
              <AnimatedLogo className="rail__mark" alt="Lightagent" width={38} height={38} />
              <PanelLeftOpen size={21} aria-hidden="true" />
            </button>
          ) : <>
            <AnimatedLogo className="rail__expanded-logo" width={34} height={34} />
            <span className="rail__wordmark">Lightagent</span>
            <button type="button" className="rail__toggle" aria-label="Collapse sidebar" title="Collapse sidebar"
              onClick={() => update({ railCollapsed: true })}><PanelLeftClose size={18} /></button>
          </>}
        </div>

        {!collapsed && (
          <div className="rail__app-actions">
            <button type="button" className="rail__back" onClick={() => navigate("/")}>
              <ArrowLeft size={17} /> Back to app
            </button>
            <button type="button" className="rail__search" onClick={() => navigate("/")}>
              <Search size={17} /> Search
            </button>
          </div>
        )}

        <div className="rail__spacer" />

        <button ref={accountButton} type="button" className="rail__account" aria-label="Open account menu"
          aria-expanded={accountOpen} onClick={() => setAccountOpen((open) => !open)}>
          <span className="rail__avatar" aria-hidden="true">LA</span>
          {!collapsed && <span className="rail__account-label"><strong>Lightagent</strong><small>Local account</small></span>}
        </button>
        <Menu open={accountOpen} anchorRef={accountButton} onClose={() => setAccountOpen(false)} minWidth={240} label="Account">
          <div className="chat-sidebar__account-menu-profile">
            <span className="chat-sidebar__avatar" aria-hidden="true">LA</span>
            <span><strong>Lightagent</strong><small>Local account</small></span>
          </div>
          <div className="menu__divider" />
          <MenuItem onClick={() => { setAccountOpen(false); navigate("/tools"); }}><span className="session-menu__item"><Wrench size={17} /> Tools</span></MenuItem>
          <MenuItem onClick={() => { setAccountOpen(false); navigate("/settings"); }}><span className="session-menu__item"><Settings size={17} /> Settings</span></MenuItem>
        </Menu>

      </nav>
      )}
      <main className={`main${chatRoute ? " main--chat" : ""}`}>
        <div className="mobilebar">
          <button type="button" className="btn btn--icon" aria-label="Open the menu"
            aria-expanded={drawerOpen} onClick={() => setDrawerOpen(true)}>
            <MenuIcon size={18} />
          </button>
          <AnimatedLogo className="rail__mark" width={26} height={26} />
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
