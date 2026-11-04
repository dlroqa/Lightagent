// Helpers for native.py, evaluated in the desktop window's own page before
// each of its scripts (desktop/src-tauri/src/e2e_driver.rs, `eval`). They
// find controls as a person sees them, by role and accessible name, visible
// ones only, and act on them through the page's own event handlers.
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const visible = (el) => !!el && el.isConnected && el.getClientRects().length > 0 && getComputedStyle(el).visibility !== "hidden";
const squash = (value) => (value || "").replace(/\s+/g, " ").trim();
const nameOf = (el) => {
  const label = el.getAttribute("aria-label");
  if (label) return squash(label);
  const by = el.getAttribute("aria-labelledby");
  if (by) return squash(by.split(/\s+/).map((id) => (document.getElementById(id) || {}).textContent || "").join(" "));
  if (el.labels && el.labels.length) return squash([...el.labels].map((item) => item.textContent).join(" "));
  return squash(el.textContent) || squash(el.getAttribute("title"));
};
const IMPLICIT = {
  BUTTON: "button", A: "link", SELECT: "combobox", TEXTAREA: "textbox", LI: "listitem", UL: "list", OL: "list",
  H1: "heading", H2: "heading", H3: "heading", H4: "heading", DIALOG: "dialog",
};
const roleOf = (el) => {
  const explicit = el.getAttribute("role");
  if (explicit) return explicit.split(/\s+/)[0];
  if (el.tagName === "INPUT") {
    const type = (el.getAttribute("type") || "text").toLowerCase();
    if (type === "radio" || type === "checkbox") return type;
    return ["button", "submit", "reset"].includes(type) ? "button" : "textbox";
  }
  return IMPLICIT[el.tagName] || "";
};
const matches = (value, want) => want === undefined || (want instanceof RegExp ? want.test(value) : value === want);
const all = (role, name, root = document) =>
  [...root.querySelectorAll("*")].filter((el) => roleOf(el) === role && visible(el) && matches(nameOf(el), name));
const one = (role, name, root) => all(role, name, root)[0] || null;
const until = async (probe, ms = 15000) => {
  const end = Date.now() + ms;
  for (;;) {
    let value = null;
    try { value = await probe(); } catch (_) { value = null; }
    if (value) return value;
    if (Date.now() > end) return null;
    await sleep(100);
  }
};
const need = async (what, probe, ms = 15000) => {
  const value = await until(probe, ms);
  if (!value) throw new Error("not found within " + ms + " ms: " + what);
  return value;
};
const press = (el) => {
  el.scrollIntoView({ block: "center", inline: "nearest" });
  if (typeof el.focus === "function") el.focus();
  el.click();
  return true;
};
const fill = (el, value) => {
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype
    : el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(proto, "value").set.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
  el.dispatchEvent(new Event("change", { bubbles: true }));
  return true;
};
const key = (el, name) => {
  el.dispatchEvent(new KeyboardEvent("keydown", { key: name, bubbles: true, cancelable: true }));
  el.dispatchEvent(new KeyboardEvent("keyup", { key: name, bubbles: true, cancelable: true }));
};
const text = () => document.body.innerText;
const describe = (el) => (el ? el.tagName.toLowerCase() + (el.id ? "#" + el.id : "") + "[" + nameOf(el).slice(0, 48) + "]" : null);
const lastBar = () => all("toolbar", "Response actions").pop() || null;
const noticeOf = (bar) => squash(((bar && bar.querySelector("[role=status]")) || {}).textContent);
