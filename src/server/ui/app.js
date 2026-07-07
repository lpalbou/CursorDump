/* CursorDump frontend.
   All transcript content is rendered via textContent (never innerHTML).

   ONE filter surface — the Finder (top bar): keyword + media chips + tools +
   scope. Any active criterion shows MESSAGE-level results (the messages that
   actually match), grouped by session. With nothing active, the centre shows
   the opened session, or the welcome screen. */
"use strict";

const MEDIA_KINDS = ["image", "audio", "video", "document", "readable"];
const MEDIA_ICON = { image: "🖼", audio: "🔊", video: "🎬", document: "📄", readable: "📎" };

const state = {
  projects: [],
  activeProject: null,     // scope for the finder + which sessions are listed
  sessions: [],            // sessions of the active project
  activeSession: null,     // path of the open session (viewer)
  expandedParents: new Set(),
  selected: new Set(),     // session paths chosen for export
  thinkingAllOpen: false,
  allTools: [],            // tool names across all projects (for the dropdown)
  finder: { query: "", media: new Set(), tools: new Set() },
  lastResults: null,       // cached /api/find response for re-render
};

/* API access token: delivered once in the opening URL (?token=…), then kept in
   sessionStorage for this tab and stripped from the address bar. Sent as a
   header on fetch calls and as a query param on media URLs (which are loaded
   via <img>/<video> and can't set headers). */
const TOKEN = (() => {
  const u = new URL(location.href);
  const t = u.searchParams.get("token");
  if (t) {
    sessionStorage.setItem("cd_token", t);
    u.searchParams.delete("token");
    history.replaceState(null, "", u.pathname + u.search + u.hash);
  }
  return sessionStorage.getItem("cd_token") || "";
})();
const mediaUrl = (path) =>
  "/api/media?path=" + encodeURIComponent(path) + "&token=" + encodeURIComponent(TOKEN);

const $ = (id) => document.getElementById(id);
const api = async (url, body) => {
  const headers = { "X-CursorDump-Token": TOKEN };
  const opts = body
    ? { method: "POST", headers: { ...headers, "Content-Type": "application/json" }, body: JSON.stringify(body) }
    : { headers };
  const r = await fetch(url, opts);
  if (r.status === 401) throw new Error("Not authorized — reopen CursorDump from the terminal link (the access token is per-session).");
  if (!r.ok) throw new Error((await r.text()) || r.statusText);
  return r.json();
};
const el = (tag, cls, text) => { const e = document.createElement(tag); if (cls) e.className = cls; if (text !== undefined) e.textContent = text; return e; };
const fmtDate = (u) => u ? new Date(u * 1000).toLocaleString(undefined, { year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" }) : "?";
const fmtSize = (b) => b >= 1048576 ? (b / 1048576).toFixed(1) + " MB" : b >= 1024 ? Math.round(b / 1024) + " KB" : b + " B";
const debounce = (fn, ms) => { let t; return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); }; };

const finderActive = () => state.finder.query.trim().length >= 2 || state.finder.media.size > 0 || state.finder.tools.size > 0;

/* ---------------- URL state ---------------- */

let applyingHash = false;
function writeHash() {
  const h = new URLSearchParams();
  if (state.activeProject) h.set("project", state.activeProject);
  if (state.activeSession) h.set("session", state.activeSession);
  if (state.finder.query.trim()) h.set("q", state.finder.query.trim());
  if (state.finder.media.size) h.set("media", [...state.finder.media].join(","));
  if (state.finder.tools.size) h.set("tools", [...state.finder.tools].join(","));
  const s = "#" + h.toString();
  if (s !== location.hash) history.replaceState(null, "", s);
}
async function applyHash() {
  applyingHash = true;
  try {
    const h = new URLSearchParams(location.hash.slice(1));
    state.finder.query = h.get("q") || "";
    $("search-input").value = state.finder.query;
    state.finder.media = new Set((h.get("media") || "").split(",").filter(Boolean));
    state.finder.tools = new Set((h.get("tools") || "").split(",").filter(Boolean));
    const proj = h.get("project");
    if (proj) await selectProject(proj, false);
    const sess = h.get("session");
    if (sess && !finderActive()) { await openSession(sess, h.get("line") ? +h.get("line") : null, false); }
    else renderCentre();
  } finally { applyingHash = false; }
  renderFinderBar();
}
window.addEventListener("hashchange", () => { if (!applyingHash) applyHash(); });

/* ---------------- projects ---------------- */

async function loadProjects() {
  const data = await api("/api/projects");
  state.projects = data.projects.filter(p => p.main_sessions + p.subagent_sessions > 0);
  renderProjects();
}
function projectName(slug) { const p = state.projects.find(p => p.slug === slug); return p ? p.display_name : slug; }

function renderProjects() {
  const list = $("project-list");
  list.replaceChildren();
  const filter = $("project-filter").value.toLowerCase();
  for (const p of state.projects) {
    if (filter && !p.display_name.toLowerCase().includes(filter) && !p.slug.toLowerCase().includes(filter)) continue;
    const row = el("div", "project-row" + (p.slug === state.activeProject ? " active" : ""));
    row.title = p.workspace_hint || p.slug;
    row.append(el("span", "name", p.display_name));
    row.append(el("span", "chip main", String(p.main_sessions)));
    if (p.subagent_sessions > 0) {
      const c = el("span", "chip sub", p.subagent_sessions + " ⋔");
      c.title = p.subagent_sessions + " subagent transcript(s)";
      row.append(c);
    }
    const exp = el("button", "row-export", "⬇");
    exp.title = "Export this whole project";
    exp.onclick = (e) => { e.stopPropagation(); exportProject(p.slug); };
    row.append(exp);
    row.onclick = () => (p.slug === state.activeProject ? deselectProject() : selectProject(p.slug));
    list.append(row);
  }
  if (!list.children.length) list.append(el("div", "empty-note", "No projects match."));
}

function deselectProject() {
  state.activeProject = null; state.sessions = []; state.activeSession = null;
  renderProjects(); renderSessions(); renderFinderBar(); renderCentre(); writeHash();
}

async function selectProject(slug, updateHash = true) {
  state.activeProject = slug;
  renderProjects();
  const data = await api("/api/sessions?project=" + encodeURIComponent(slug));
  state.sessions = data.sessions;
  renderSessions();
  renderFinderBar();          // scope chip reflects the project
  renderCentre();             // finder results re-scope, or show welcome
  if (updateHash) writeHash();
}

/* ---------------- sessions (browse) ---------------- */

function renderSessions() {
  const list = $("session-list");
  list.replaceChildren();
  const has = state.activeProject !== null;
  if (!has) { list.append(el("div", "empty-note", "Pick a project.")); return; }
  const filter = $("session-filter").value.toLowerCase();
  const parents = state.sessions.filter(s => !s.is_subagent);
  const children = {};
  for (const s of state.sessions.filter(s => s.is_subagent)) (children[s.parent_id] = children[s.parent_id] || []).push(s);
  const matches = (s) => !filter || s.title.toLowerCase().includes(filter) || s.id.includes(filter);
  let n = 0; state._visible = [];
  for (const p of parents) {
    const subs = children[p.id] || [];
    if (!matches(p) && !subs.some(matches)) continue;
    list.append(sessionRow(p, subs.length)); state._visible.push(p); n++;
    if (state.expandedParents.has(p.path)) for (const c of subs) { list.append(sessionRow(c, 0)); state._visible.push(c); n++; }
  }
  if (!n) list.append(el("div", "empty-note", "No sessions match."));
  updateSelectionSummary();
}

function updateSelectionSummary() {
  const n = state.selected.size;
  $("selection-summary").textContent = n === 0 ? "0 selected" : `${n} selected`;
  $("export-btn").disabled = n === 0;
}

function sessionRow(s, subCount) {
  const row = el("div", "session-row" + (s.is_subagent ? " sub" : "") + (s.path === state.activeSession ? " active" : ""));
  const cb = el("input"); cb.type = "checkbox"; cb.checked = state.selected.has(s.path); cb.title = "Select for export";
  cb.onchange = () => { cb.checked ? state.selected.add(s.path) : state.selected.delete(s.path); updateSelectionSummary(); };
  row.append(cb);
  const main = el("div", "session-main");
  main.append(el("div", "session-title", s.title));
  const info = el("div", "session-info");
  info.append(el("span", null, fmtDate(s.modified_unix)));
  info.append(el("span", null, fmtSize(s.size_bytes)));
  if (subCount > 0) {
    const open = state.expandedParents.has(s.path);
    const t = el("button", "sub-toggle", (open ? "▾ " : "▸ ") + subCount + " subagents");
    t.onclick = (ev) => { ev.stopPropagation(); open ? state.expandedParents.delete(s.path) : state.expandedParents.add(s.path); renderSessions(); };
    info.append(t);
  }
  main.append(info);
  main.onclick = () => openSession(s.path);
  row.append(main);
  return row;
}

/* ---------------- the Finder bar ---------------- */

function renderFinderBar() {
  // Media chips (fixed set, always visible).
  const mc = $("media-chips");
  mc.replaceChildren();
  for (const k of MEDIA_KINDS) {
    const on = state.finder.media.has(k);
    const chip = el("button", "facet-chip" + (on ? " on" : ""), MEDIA_ICON[k] + " " + k);
    chip.onclick = () => { on ? state.finder.media.delete(k) : state.finder.media.add(k); runFinder(); };
    mc.append(chip);
  }
  // Tools dropdown button reflects count.
  const tb = $("tools-btn");
  tb.textContent = state.finder.tools.size ? `🔧 Tools (${state.finder.tools.size}) ▾` : "🔧 Tools ▾";
  tb.classList.toggle("on", state.finder.tools.size > 0);
  // Scope chip.
  const sc = $("scope-chip");
  sc.replaceChildren();
  if (state.activeProject) {
    const chip = el("span", "scope", "◉ " + projectName(state.activeProject));
    const x = el("button", "scope-x", "✕"); x.title = "Search all projects"; x.onclick = () => deselectProject();
    chip.append(x); sc.append(chip);
  } else {
    sc.append(el("span", "scope-all", "all projects"));
  }
  $("finder-clear").hidden = !finderActive();
}

function renderToolsMenu() {
  const menu = $("tools-menu");
  menu.replaceChildren();
  if (!state.allTools.length) { menu.append(el("div", "empty-note", "loading…")); return; }
  for (const t of state.allTools) {
    const on = state.finder.tools.has(t);
    const item = el("label", "tool-item");
    const cb = el("input"); cb.type = "checkbox"; cb.checked = on;
    cb.onchange = () => { cb.checked ? state.finder.tools.add(t) : state.finder.tools.delete(t); renderFinderBar(); runFinder(); };
    item.append(cb, el("span", null, t));
    menu.append(item);
  }
}

async function ensureTools() {
  if (state.allTools.length) return;
  try { const f = await api("/api/facets"); state.allTools = f.tools; } catch (_) {}
  renderToolsMenu();
}

const runFinderDebounced = debounce(() => runFinder(), 250);

async function runFinder(updateHash = true) {
  renderFinderBar();
  if (updateHash) writeHash();
  if (!finderActive()) { state.lastResults = null; renderCentre(); return; }
  const centre = $("viewer");
  centre.replaceChildren(el("div", "empty-note", "Finding…"));
  const body = {
    query: state.finder.query.trim(),
    media: [...state.finder.media],
    tools: [...state.finder.tools],
    project: state.activeProject || null,
  };
  let data;
  try { data = await api("/api/find", body); }
  catch (e) { centre.replaceChildren(el("div", "empty-note", "Find failed: " + e.message)); return; }
  state.lastResults = data;
  renderResults(data);
}

/* Central panel router. */
function renderCentre() {
  if (finderActive()) { if (state.lastResults) renderResults(state.lastResults); else runFinder(false); return; }
  if (state.activeSession) return; // viewer already shown by openSession
  renderWelcome();
}

/* ---------------- results (message-level, grouped by session) ---------------- */

function renderResults(data) {
  const v = $("viewer");
  v.replaceChildren();
  const head = el("div", "viewer-head");
  head.append(el("h1", null, describeQuery(data.total)));
  head.append(el("div", "muted", data.truncated ? `showing first 500 of ${data.total}` : `${data.total} message(s) matched`));
  v.append(head);
  if (!data.results.length) {
    v.append(el("div", "empty-note", "No messages match. Adjust the keyword or filters above."));
    return;
  }
  // Group consecutive results by session (backend already sorted by recency).
  const groups = [];
  let cur = null;
  for (const r of data.results) {
    if (!cur || cur.path !== r.session_path) { cur = { path: r.session_path, title: r.session_title, project: r.project, project_name: r.project_name, sub: r.is_subagent, items: [] }; groups.push(cur); }
    cur.items.push(r);
  }
  for (const g of groups) {
    const gEl = el("div", "result-group");
    const gh = el("div", "result-group-head");
    gh.append(el("span", "chip main", g.project_name));
    if (g.sub) gh.append(el("span", "chip sub", "subagent"));
    gh.append(el("span", "rg-title", g.title));
    gh.append(el("span", "rg-count", g.items.length + " match" + (g.items.length > 1 ? "es" : "")));
    gEl.append(gh);
    const SHOWN = 4;
    const render = (all) => {
      [...gEl.querySelectorAll(".result-card,.rg-more")].forEach(n => n.remove());
      const items = all ? g.items : g.items.slice(0, SHOWN);
      for (const r of items) gEl.append(resultCard(r));
      if (!all && g.items.length > SHOWN) {
        const more = el("button", "rg-more", `show ${g.items.length - SHOWN} more in this session ▾`);
        more.onclick = () => render(true);
        gEl.append(more);
      }
    };
    render(false);
    v.append(gEl);
  }
}

function describeQuery(total) {
  const parts = [];
  if (state.finder.query.trim()) parts.push(`“${state.finder.query.trim()}”`);
  if (state.finder.media.size) parts.push("has " + [...state.finder.media].join("/"));
  if (state.finder.tools.size) parts.push("uses " + [...state.finder.tools].join("/"));
  const scope = state.activeProject ? " in " + projectName(state.activeProject) : "";
  return "Find: " + parts.join(" · ") + scope;
}

function resultCard(r) {
  const card = el("div", "result-card");
  card.onclick = () => openSession(r.session_path, r.line_index);
  const head = el("div", "msg-head");
  head.append(el("span", "badge " + r.role, r.role.toUpperCase()));
  head.append(el("span", "msg-idx", "#" + r.line_index));
  if (r.media && r.media.length) head.append(el("span", "msg-idx", "📎 " + r.media.length));
  if (r.tools && r.tools.length) for (const t of r.tools.slice(0, 4)) head.append(el("span", "tool-mini", t));
  card.append(head);
  if (r.snippet) { const s = el("div", "snippet"); fillHighlighted(s, r.snippet, state.finder.query.trim()); card.append(s); }
  const imgs = (r.media || []).filter(m => m.kind === "image");
  const others = (r.media || []).filter(m => m.kind !== "image");
  if (imgs.length || others.length) {
    const strip = el("div", "result-media");
    for (const m of imgs.slice(0, 5)) {
      if (!m.available) { strip.append(el("span", "thumb missing", "🚫")); continue; }
      const url = mediaUrl(m.path);
      const t = el("img", "thumb"); t.src = url; t.loading = "lazy"; t.alt = m.name; t.title = m.name;
      t.onclick = (e) => { e.stopPropagation(); openLightbox(url, m.name); };
      strip.append(t);
    }
    if (imgs.length > 5) strip.append(el("span", "thumb more", "+" + (imgs.length - 5)));
    for (const m of others.slice(0, 4)) {
      const icon = m.kind === "video" ? "🎬" : m.kind === "audio" ? "🔊" : m.kind === "document" ? "📄" : "📎";
      const chip = el("span", "attach-chip" + (m.available ? "" : " missing"), icon + " " + m.name);
      strip.append(chip);
    }
    card.append(strip);
  }
  return card;
}

/* ---------------- viewer ---------------- */

async function openSession(path, focusLine, updateHash = true) {
  state.activeSession = path;
  const v = $("viewer");
  v.replaceChildren(el("div", "empty-note", "Loading…"));
  let data;
  try { data = await api("/api/session?path=" + encodeURIComponent(path)); }
  catch (e) { v.replaceChildren(el("div", "empty-note", "Failed to load session: " + e.message)); return; }
  v.replaceChildren();
  const head = el("div", "viewer-head");
  const titleRow = el("div", "title-row");
  if (state.lastResults && finderActive()) { const back = el("button", "btn small", "← results"); back.onclick = () => { state.activeSession = null; renderResults(state.lastResults); }; titleRow.append(back); }
  titleRow.append(el("h1", null, data.meta.title));
  head.append(titleRow);
  const badges = el("div", "viewer-badges");
  if (data.meta.is_subagent) badges.append(el("span", "badge subagent", "SUBAGENT"));
  badges.append(el("span", "chip", data.turns + " turns"));
  badges.append(el("span", "chip", data.messages.length + " messages"));
  if (data.skipped_lines > 0) badges.append(el("span", "chip warn", data.skipped_lines + " unparseable"));
  badges.append(el("span", "chip", fmtDate(data.meta.modified_unix)));
  if (data.messages.some(m => m.thinking && m.thinking.trim())) {
    const tb = el("button", "btn small", "💭 expand all thinking");
    tb.onclick = () => { state.thinkingAllOpen = !state.thinkingAllOpen; tb.textContent = state.thinkingAllOpen ? "💭 collapse all thinking" : "💭 expand all thinking"; document.querySelectorAll(".thinking-block").forEach(b => b._setOpen(state.thinkingAllOpen)); };
    badges.append(tb);
  }
  head.append(badges);
  v.append(head);
  const q = finderActive() ? state.finder.query.trim() : null;
  let focusEl = null;
  for (let i = 0; i < data.messages.length; i++) {
    const node = messageCard(data.messages[i], i, q);
    v.append(node);
    if (focusLine != null && data.messages[i].line_index === focusLine) focusEl = node;
  }
  if (focusEl) { focusEl.scrollIntoView({ block: "center" }); focusEl.classList.add("flash"); setTimeout(() => focusEl.classList.remove("flash"), 1600); }
  if (updateHash) { const h = new URLSearchParams(location.hash.slice(1)); h.set("session", path); if (focusLine != null) h.set("line", String(focusLine)); location.hash = "#" + h.toString(); }
}

const PREVIEW = 1600;

function messageCard(m, idx, highlight) {
  const card = el("div", "msg");
  const head = el("div", "msg-head");
  head.append(el("span", "badge " + m.role, m.role.toUpperCase()));
  head.append(el("span", "msg-idx", "#" + idx));
  if (m.role === "user" && m.injected) {
    const b = el("span", "badge injected", "system-injected");
    b.title = "Harness-injected record (subagent/background notification), not typed by you. Excluded from clean exports.";
    head.append(b);
  }
  if (m.media && m.media.length) head.append(el("span", "msg-idx", "📎 " + m.media.length));
  card.append(head);

  if (m.thinking && m.thinking.trim()) {
    const block = el("div", "thinking-block");
    const label = () => "💭 thinking · " + m.thinking.length.toLocaleString() + " chars";
    const btn = el("button", "thinking-toggle");
    const content = el("div", "thinking-content"); fillHighlighted(content, m.thinking, highlight); content.hidden = true;
    let open = state.thinkingAllOpen || (highlight && m.thinking.toLowerCase().includes(highlight.toLowerCase()));
    const apply = () => { content.hidden = !open; btn.textContent = label() + (open ? " ▾" : " ▸"); };
    block._setOpen = (o) => { open = o; apply(); };
    btn.onclick = () => { open = !open; apply(); };
    apply(); block.append(btn, content); card.append(block);
  }

  const text = m.role === "user" ? (m.query || m.raw) : m.answer;
  if (text && text.trim()) card.append(bodyNode(text.trim(), highlight));
  if (m.media && m.media.length) card.append(attachmentsNode(m.media));

  if (m.tools.length) {
    const tools = el("div", "tools");
    const toolWrap = el("div", "tool-details");
    for (const t of m.tools) {
      const chip = el("button", "tool-chip", "🔧 " + toolSummary(t) + " ▸");
      chip.title = "Click to view the tool INPUT (transcripts don't record tool outputs)";
      const detail = el("div", "tool-detail");
      detail.append(el("div", "tool-detail-label", "tool input — outputs are not recorded in transcripts"));
      const pre = el("pre"); pre.textContent = JSON.stringify(t.input, null, 2); detail.append(pre);
      detail.hidden = true;
      chip.onclick = () => { detail.hidden = !detail.hidden; chip.textContent = "🔧 " + toolSummary(t) + (detail.hidden ? " ▸" : " ▾"); chip.classList.toggle("active", !detail.hidden); };
      tools.append(chip); toolWrap.append(detail);
    }
    card.append(tools, toolWrap);
  }
  return card;
}

function openLightbox(url, name) {
  const ov = el("div", "lightbox");
  const img = el("img"); img.src = url; img.alt = name;
  const cap = el("div", "lightbox-cap"); cap.append(document.createTextNode(name + "  "));
  const raw = el("a", null, "open raw ↗"); raw.href = url; raw.target = "_blank"; cap.append(raw);
  ov.append(img, cap);
  const close = () => ov.remove();
  ov.onclick = (e) => { if (e.target !== raw) close(); };
  const esc = (e) => { if (e.key === "Escape") { close(); document.removeEventListener("keydown", esc); } };
  document.addEventListener("keydown", esc);
  document.body.append(ov);
}

function attachmentsNode(media) {
  const wrap = el("div", "attachments");
  for (const a of media) {
    const url = mediaUrl(a.path);
    if (!a.available) { const c = el("span", "attach-chip missing", a.name + " (missing)"); c.title = a.path; wrap.append(c); continue; }
    if (a.kind === "image") {
      const link = el("a", "attach-img-link"); link.href = url; link.title = a.name; link.setAttribute("data-name", a.name);
      const img = el("img", "attach-img"); img.src = url; img.alt = a.name; img.loading = "lazy"; link.append(img);
      link.onclick = (e) => { e.preventDefault(); openLightbox(url, a.name); };
      wrap.append(link);
    } else if (a.kind === "video") {
      const vd = el("video", "attach-video"); vd.src = url; vd.controls = true; vd.preload = "none"; wrap.append(vd);
    } else if (a.kind === "audio") {
      const box = el("div", "attach-audio"); box.append(el("div", "attach-name", "🔊 " + a.name));
      const au = el("audio"); au.src = url; au.controls = true; au.preload = "none"; box.append(au); wrap.append(box);
    } else {
      const icon = a.kind === "document" ? "📄" : "📎";
      const chip = el("a", "attach-chip", icon + " " + a.name); chip.href = url; chip.target = "_blank"; chip.title = a.path; wrap.append(chip);
    }
  }
  return wrap;
}

function toolSummary(t) {
  for (const k of ["path", "command", "query", "pattern", "url", "glob_pattern", "description"]) {
    const v = t.input && t.input[k];
    if (typeof v === "string") return t.name + ": " + (v.length > 66 ? v.slice(0, 66) + "…" : v);
  }
  return t.name;
}

function bodyNode(text, highlight) {
  const body = el("div", "msg-body");
  const long = text.length > PREVIEW;
  const render = (full) => {
    body.replaceChildren();
    const shown = full ? text : safeTruncate(text, PREVIEW);
    const parts = shown.split("```");
    for (let i = 0; i < parts.length; i++) {
      if (i % 2 === 0) { if (parts[i]) fillHighlighted(body, parts[i], highlight); }
      else { const pre = el("pre"); pre.textContent = parts[i].replace(/^[a-z0-9_+-]*\n/i, ""); body.append(pre); }
    }
    if (long) { const btn = el("button", "expand-link", full ? "▴ collapse" : `▾ show all ${text.length.toLocaleString()} chars`); btn.onclick = () => { render(!full); if (full) body.scrollIntoView({ block: "nearest" }); }; body.append(el("div"), btn); }
  };
  render(!long);
  return body;
}
function safeTruncate(text, max) {
  if (text.length <= max) return text;
  const head = text.slice(0, max);
  const lastFence = head.lastIndexOf("```");
  if ((head.match(/```/g) || []).length % 2 === 1 && lastFence > 0) return text.slice(0, lastFence).trimEnd();
  const nl = head.lastIndexOf("\n");
  return nl > max * 0.6 ? head.slice(0, nl) : head;
}
function fillHighlighted(node, text, term) {
  if (!term) { node.append(document.createTextNode(text)); return; }
  const lower = text.toLowerCase(), t = term.toLowerCase();
  let i = 0, idx;
  while ((idx = lower.indexOf(t, i)) !== -1) { if (idx > i) node.append(document.createTextNode(text.slice(i, idx))); node.append(el("mark", null, text.slice(idx, idx + t.length))); i = idx + t.length; }
  if (i < text.length) node.append(document.createTextNode(text.slice(i)));
}

/* ---------------- welcome ---------------- */

function renderWelcome() {
  const v = $("viewer");
  v.replaceChildren();
  const w = el("div", "welcome");
  w.append(el("div", "logo", "◆"));
  w.append(el("h1", null, "CursorDump"));
  const steps = el("div", "steps");
  const step = (n, t, d, onClick) => {
    const s = el("button", "step");
    s.type = "button";
    s.append(el("span", "n", n));
    const tx = el("div", "step-text");
    tx.append(el("strong", null, t));
    tx.append(el("small", null, d));
    s.append(tx);
    s.onclick = onClick;
    return s;
  };
  steps.append(step("🔍", "Find messages",
    "type a keyword and/or toggle the 🖼 media and 🔧 tool chips in the bar — results are the exact messages that match, with their attachments",
    () => $("search-input").focus()));
  steps.append(step("🗂", "Browse projects",
    "pick a project (left) → a session (middle); that also scopes the finder to it",
    () => { $("project-filter").focus(); pulse($("projects-pane")); }));
  steps.append(step("⬇", "Export",
    "tick sessions and export SFT/CPT datasets",
    () => openExportDialog()));
  steps.append(step("🗄", "Backup",
    "a full, Cursor-independent copy of everything",
    () => openBackupDialog()));
  w.append(steps);
  v.append(w);
}

/* Briefly highlight a pane to guide the eye after a welcome-step click. */
function pulse(node) {
  if (!node) return;
  node.classList.remove("pulse");
  void node.offsetWidth; // restart the animation
  node.classList.add("pulse");
  setTimeout(() => node.classList.remove("pulse"), 1200);
}

/* ---------------- export / backup (dialogs) ---------------- */

async function exportProject(slug) {
  if (state.activeProject !== slug) await selectProject(slug, false);
  for (const s of state.sessions) if (!s.is_subagent) state.selected.add(s.path);
  renderSessions();
  openExportDialog();
}

async function openExportDialog() {
  const n = state.selected.size;
  $("export-count").textContent = n ? `${n} session(s) selected for export` : "no sessions selected — tick some, or use a project's ⬇";
  $("export-result").replaceChildren();
  if (!$("o-outdir").value) $("o-outdir").value = (await api("/api/default_out_dir")).path;
  $("export-dialog").showModal();
}

async function doExport() {
  const btn = $("do-export"), res = $("export-result");
  const paths = [...state.selected];
  if (!paths.length) { res.replaceChildren(el("div", "err", "Select at least one session.")); return; }
  btn.disabled = true; res.replaceChildren(el("span", "spinner"), el("span", null, " exporting…"));
  const body = { paths, out_dir: $("o-outdir").value.trim(), options: {
    sft_chatml: $("o-sft-chatml").checked, sft_sharegpt: $("o-sft-sharegpt").checked,
    cpt_jsonl: $("o-cpt-jsonl").checked, cpt_txt: $("o-cpt-txt").checked,
    include_tool_calls: $("o-tools").checked, thinking: document.querySelector('input[name="thinking"]:checked').value,
    subagent_mode: document.querySelector('input[name="subagents"]:checked').value,
    clean_assistant: $("o-clean").checked, final_response_only: $("o-final-only").checked,
    user_content: $("o-raw-user").checked ? "raw" : "clean", copy_media: $("o-media").checked,
    inline_readable_attachments: $("o-inline-attach").checked, with_metadata: $("o-metadata").checked,
    redact_secrets: $("o-redact").checked,
    val_fraction: parseFloat($("o-val").value) || 0,
  }};
  try {
    const r = await api("/api/export", body);
    res.replaceChildren();
    res.append(el("div", "ok", `✓ Exported ${r.sessions_exported} session(s) → ${r.out_dir}`));
    if (r.selected_subagents > 0 && body.options.subagent_mode === "inline") res.append(el("div", "muted", `${r.selected_subagents} subagent(s) inlined into masters.`));
    res.append(el("div", null, `${r.sft_records} SFT record(s), ${r.cpt_records} CPT record(s), media ${r.media_copied}/${r.media_referenced} copied${r.sessions_skipped ? `, ${r.sessions_skipped} skipped` : ""}`));
    if (r.warnings.length) { const ul = el("ul"); for (const w of r.warnings.slice(0, 6)) ul.append(el("li", null, w)); res.append(ul); }
  } catch (e) { res.replaceChildren(el("div", "err", "✖ " + e.message)); }
  finally { btn.disabled = false; }
}

async function openBackupDialog() {
  const cur = state.activeProject ? projectName(state.activeProject) : "none";
  $("bk-current").textContent = cur;
  document.querySelector('input[name="bk-scope"][value="current"]').disabled = !state.activeProject;
  $("backup-result").replaceChildren();
  if (!$("bk-outdir").value) $("bk-outdir").value = (await api("/api/default_backup_dir")).path;
  $("backup-dialog").showModal();
}

async function doBackup() {
  const btn = $("do-backup"), res = $("backup-result");
  btn.disabled = true; res.replaceChildren(el("span", "spinner"), el("span", null, " backing up… (large media may take a while)"));
  const scope = document.querySelector('input[name="bk-scope"]:checked').value;
  const body = { projects: scope === "current" && state.activeProject ? [state.activeProject] : [], out_dir: $("bk-outdir").value.trim(), skip_runtime: $("bk-skip-runtime").checked, verify_transcripts: $("bk-verify").checked, include_app: $("bk-include-app").checked, include_external_attachments: $("bk-attachments").checked };
  try {
    const r = await api("/api/backup", body);
    res.replaceChildren();
    res.append(el("div", "ok", `✓ Backed up ${r.projects} project(s) → ${r.out_dir}`));
    res.append(el("div", null, `${r.files_copied} file(s) copied, ${r.files_unchanged} unchanged, ${(r.bytes_total/1048576).toFixed(1)} MB total`));
    if (body.include_app) res.append(el("div", "muted", "Self-contained: run ./cursordump projects inside the backup to re-explore it without Cursor."));
    res.append(el("div", "muted", "Restore later with: cp -a projects/* ~/.cursor/projects/"));
    if (r.warnings.length) { const ul = el("ul"); for (const w of r.warnings.slice(0, 6)) ul.append(el("li", null, w)); res.append(ul); }
  } catch (e) { res.replaceChildren(el("div", "err", "✖ " + e.message)); }
  finally { btn.disabled = false; }
}

/* ---------------- keyboard ---------------- */

document.addEventListener("keydown", (e) => {
  if (e.key === "/" && document.activeElement !== $("search-input")) { e.preventDefault(); $("search-input").focus(); }
  else if (e.key === "Escape" && document.activeElement === $("search-input")) $("search-input").blur();
});

/* ---------------- wiring ---------------- */

$("project-filter").oninput = renderProjects;
$("session-filter").oninput = renderSessions;
$("search-input").addEventListener("input", (e) => { state.finder.query = e.target.value; runFinderDebounced(); });
$("search-input").addEventListener("keydown", (e) => { if (e.key === "Enter") runFinder(); });
$("finder-clear").onclick = () => {
  state.finder = { query: "", media: new Set(), tools: new Set() };
  $("search-input").value = "";
  renderToolsMenu(); renderFinderBar();
  state.lastResults = null;
  if (state.activeSession) openSession(state.activeSession, null); else renderWelcome();
  writeHash();
};
$("tools-btn").onclick = () => { const m = $("tools-menu"); m.hidden = !m.hidden; if (!m.hidden) { ensureTools(); renderToolsMenu(); } };
document.addEventListener("click", (e) => { if (!e.target.closest(".tools-dd")) $("tools-menu").hidden = true; });
$("rescan-btn").onclick = async () => { await api("/api/rescan", {}); state.allTools = []; await loadProjects(); if (state.activeProject) selectProject(state.activeProject, false); };
$("select-all-btn").onclick = () => { for (const s of (state._visible || [])) state.selected.add(s.path); renderSessions(); };
$("clear-proj-btn").onclick = () => { for (const s of state.sessions) state.selected.delete(s.path); renderSessions(); };
$("export-btn").onclick = () => openExportDialog();
$("do-export").onclick = doExport;
$("close-dialog").onclick = () => $("export-dialog").close();
$("backup-btn").onclick = openBackupDialog;
$("do-backup").onclick = doBackup;
$("close-backup").onclick = () => $("backup-dialog").close();
document.querySelectorAll(".preset").forEach(b => b.onclick = () => applyPreset(b.dataset.preset));

const PRESETS = {
  chat: { chatml: true, sharegpt: false, cptj: false, cptt: false, tools: false, thinking: "tagged", subagents: "inline" },
  agentic: { chatml: true, sharegpt: false, cptj: false, cptt: false, tools: true, thinking: "tagged", subagents: "inline" },
  cpt: { chatml: false, sharegpt: false, cptj: true, cptt: true, tools: false, thinking: "verbatim", subagents: "inline" },
  all: { chatml: true, sharegpt: true, cptj: true, cptt: true, tools: false, thinking: "tagged", subagents: "inline" },
};
function applyPreset(name) {
  const p = PRESETS[name];
  $("o-sft-chatml").checked = p.chatml; $("o-sft-sharegpt").checked = p.sharegpt;
  $("o-cpt-jsonl").checked = p.cptj; $("o-cpt-txt").checked = p.cptt; $("o-tools").checked = p.tools;
  document.querySelector(`input[name="thinking"][value="${p.thinking}"]`).checked = true;
  document.querySelector(`input[name="subagents"][value="${p.subagents}"]`).checked = true;
  document.querySelectorAll(".preset").forEach(b => b.classList.toggle("active", b.dataset.preset === name));
}

renderWelcome();
renderFinderBar();
ensureTools();
loadProjects().then(applyHash);
