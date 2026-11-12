import { api, type Analysis, type DriveInfo, type Entry, type ItemResult, type LinkKind, type LinkOption, type Listing, type Mode, type Progress, type SizeInfo } from "./api";
import { icons, kindIcon } from "./icons";

// ---------------------------------------------------------------- helpers

const $ = <T extends HTMLElement = HTMLElement>(sel: string, root: ParentNode = document) => root.querySelector(sel) as T;
const ESC: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
const esc = (s: string) => s.replace(/[&<>"']/g, (c) => ESC[c]);
const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });
const dateFmt = new Intl.DateTimeFormat(undefined, { dateStyle: "short", timeStyle: "short" });

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const u = ["KB", "MB", "GB", "TB", "PB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${u[i]}`;
}

/** Parent folder of a path string (Windows or Unix); null at a drive / filesystem root. */
function parentOf(p: string): string | null {
  let s = p;
  if (s.length > 1 && /[\\/]$/.test(s) && !/^[A-Za-z]:[\\/]$/.test(s)) s = s.slice(0, -1);
  if (/^[A-Za-z]:[\\/]?$/.test(s) || s === "/") return null;
  const i = Math.max(s.lastIndexOf("\\"), s.lastIndexOf("/"));
  if (i < 0) return null;
  let par = s.slice(0, i);
  if (/^[A-Za-z]:$/.test(par)) par += "\\";
  if (par === "") par = "/";
  return par;
}

const baseName = (p: string) => p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || p;

const KIND_LABEL: Record<LinkKind, string> = {
  junction: "Junction",
  symlink: "Symbolic link",
  hardlink: "Hard link",
  clone: "Clone",
  bind: "Bind mount",
  snapshot: "Snapshot",
};

function linkClass(e: Entry): string {
  if (!e.link) return "";
  if (e.link.kind !== "hardlink" && !e.link.targetExists) return "broken";
  return e.link.kind;
}

function typeLabel(e: Entry): string {
  if (e.link) {
    if (e.link.kind === "hardlink") return `Hard link ×${e.link.linkCount ?? 2}`;
    if (!e.link.targetExists) return "Broken link";
    return KIND_LABEL[e.link.kind];
  }
  if (e.isDir) return "Folder";
  const dot = e.name.lastIndexOf(".");
  return dot > 0 ? `${e.name.slice(dot + 1).toUpperCase()} file` : "File";
}

let statusTimer = 0;
function setStatus(msg: string, err = false) {
  const el = $("#status-msg");
  el.textContent = msg;
  el.classList.toggle("err", err);
  clearTimeout(statusTimer);
  statusTimer = window.setTimeout(() => (el.textContent = ""), 7000);
}

// ---------------------------------------------------------------- app state

const app = {
  platform: "windows",
  home: null as string | null,
  showHidden: false,
  calcSizes: true,
  followLinks: true,
  mode: "move" as Mode,
  kind: null as LinkKind | null,
  userPickedKind: false,
  detailKind: null as LinkKind | null,
  analysis: null as Analysis | null,
  active: 0,
  dragging: null as string[] | null,
  lastPointer: "mouse",
};

type SortKey = "name" | "size" | "modified" | "type";
type SizeState = SizeInfo | "pending" | "error";

// ---------------------------------------------------------------- pane

class Pane {
  path: string | null = null;
  listing: Listing | null = null;
  drives: DriveInfo[] = [];
  rows: Entry[] = [];
  selected = new Set<string>();
  anchor: string | null = null;
  focus: string | null = null;
  sort: { key: SortKey; dir: 1 | -1 } = { key: "name", dir: 1 };
  sizes = new Map<string, SizeState>();
  back: (string | null)[] = [];
  fwd: (string | null)[] = [];
  loaded = false;
  navToken = 0;
  queue: string[] = [];
  running = 0;
  epoch = 0;
  renderQueued = false;
  els: Record<string, HTMLElement> = {};

  constructor(public id: number, public el: HTMLElement, public role: "source" | "dest") {
    this.build();
  }

  // ---- DOM
  build() {
    const src = this.role === "source";
    this.el.dataset.role = this.role;
    this.el.innerHTML = `
      <div class="pane-head">
        <div class="pane-title">
          <span class="role">${src ? "SOURCE" : "DESTINATION"}</span>
          <span class="role-sub">${src ? "the real folders / files you want to move or link" : "where they should go"}</span>
          <span class="pane-fs"></span>
        </div>
        <div class="toolbar">
          <button class="tb" data-act="back" title="Back (Alt+←)">${icons.back()}</button>
          <button class="tb" data-act="fwd" title="Forward (Alt+→)">${icons.forward()}</button>
          <button class="tb" data-act="up" title="Up one level (Backspace)">${icons.up()}</button>
          <button class="tb" data-act="pc" title="This Computer">${icons.computer()}</button>
          <div class="addr" title="Click to type a path"><div class="crumbs"></div><input class="hidden" spellcheck="false" /></div>
          <button class="tb" data-act="reveal" title="Show in file manager">${icons.reveal()}</button>
          <button class="tb" data-act="refresh" title="Refresh (F5)">${icons.refresh()}</button>
        </div>
      </div>
      <div class="pane-body">
        <div class="drives hidden"></div>
        <table class="files hidden">
          <thead><tr>
            <th class="c-check" data-act="selall" title="Select all"><span class="cb">${icons.check(12)}</span></th>
            <th data-sort="name">Name</th>
            <th class="c-size" data-sort="size">Size</th>
            <th class="c-mod" data-sort="modified">Modified</th>
            <th class="c-type" data-sort="type">Type</th>
          </tr></thead>
          <tbody></tbody>
        </table>
        <div class="empty hidden"></div>
      </div>
      <div class="pane-foot"></div>`;
    this.els = {
      fs: $(".pane-fs", this.el),
      addr: $(".addr", this.el),
      crumbs: $(".crumbs", this.el),
      input: $("input", this.el),
      body: $(".pane-body", this.el),
      drives: $(".drives", this.el),
      table: $("table", this.el),
      thead: $("thead", this.el),
      tbody: $("tbody", this.el),
      empty: $(".empty", this.el),
      foot: $(".pane-foot", this.el),
      selall: $("th.c-check .cb", this.el),
    };
    this.wire();
  }

  wire() {
    const { addr, crumbs, input, thead, tbody, drives, body } = this.els;

    this.el.addEventListener("pointerdown", (e) => {
      app.lastPointer = e.pointerType;
      setActive(this.id);
    });

    $(".toolbar", this.el).addEventListener("click", (e) => {
      const act = (e.target as HTMLElement).closest<HTMLElement>("[data-act]")?.dataset.act;
      if (act === "back") this.goBack();
      else if (act === "fwd") this.goForward();
      else if (act === "up") this.up();
      else if (act === "pc") this.navigate(null);
      else if (act === "refresh") this.refresh();
      else if (act === "reveal") {
        const p = this.selected.size === 1 ? [...this.selected][0] : this.path;
        if (p) api.reveal(p).catch((err) => setStatus(String(err), true));
      }
    });

    crumbs.addEventListener("click", (e) => {
      const c = (e.target as HTMLElement).closest<HTMLElement>("[data-path]");
      if (c) {
        e.stopPropagation();
        this.navigate(c.dataset.path || null); // empty = This PC
      }
    });
    addr.addEventListener("click", () => this.editPath());
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        const v = (input as HTMLInputElement).value.trim();
        this.stopEdit();
        if (v) this.navigate(v);
      } else if (e.key === "Escape") this.stopEdit();
      e.stopPropagation();
    });
    input.addEventListener("blur", () => this.stopEdit());

    thead.addEventListener("click", (e) => {
      const th = (e.target as HTMLElement).closest<HTMLElement>("th");
      if (!th) return;
      if (th.dataset.act === "selall") {
        const all = this.selected.size === this.rows.length && this.rows.length > 0;
        this.selected = all ? new Set() : new Set(this.rows.map((r) => r.path));
        this.selectionChanged();
        return;
      }
      const key = th.dataset.sort as SortKey | undefined;
      if (!key) return;
      if (this.sort.key === key) this.sort.dir = this.sort.dir === 1 ? -1 : 1;
      else this.sort = { key, dir: key === "name" || key === "type" ? 1 : -1 };
      this.computeRows();
      this.render();
    });

    tbody.addEventListener("click", (e) => this.onRowClick(e as MouseEvent));
    tbody.addEventListener("dblclick", (e) => {
      if (app.lastPointer !== "mouse") return;
      const tr = (e.target as HTMLElement).closest<HTMLElement>("tr");
      if (!tr || (e.target as HTMLElement).closest(".c-check")) return;
      if (tr.dataset.up !== undefined) return; // handled on single click
      const entry = this.rows[Number(tr.dataset.i)];
      if (entry) this.open(entry);
    });

    drives.addEventListener("click", (e) => {
      const d = (e.target as HTMLElement).closest<HTMLElement>("[data-mount]");
      if (d) this.navigate(d.dataset.mount!);
    });

    // drag (source)
    tbody.addEventListener("dragstart", (e) => this.onDragStart(e));
    tbody.addEventListener("dragend", () => endDrag());

    // drop (destination)
    if (this.role === "dest") {
      body.addEventListener("dragover", (e) => this.onDragOver(e));
      body.addEventListener("dragleave", (e) => {
        if (!body.contains(e.relatedTarget as Node)) clearDropMarks();
      });
      body.addEventListener("drop", (e) => this.onDrop(e));
    }
  }

  editPath() {
    const { addr, crumbs, input } = this.els;
    if (addr.classList.contains("editing")) return;
    addr.classList.add("editing");
    crumbs.classList.add("hidden");
    input.classList.remove("hidden");
    (input as HTMLInputElement).value = this.path ?? "";
    input.focus();
    (input as HTMLInputElement).select();
  }

  stopEdit() {
    const { addr, crumbs, input } = this.els;
    addr.classList.remove("editing");
    crumbs.classList.remove("hidden");
    input.classList.add("hidden");
  }

  // ---- navigation
  async navigate(path: string | null, opts: { push?: boolean; highlight?: string | null } = {}) {
    const push = opts.push ?? true;
    const token = ++this.navToken;
    let listing: Listing | null = null;
    let drives: DriveInfo[] = [];
    try {
      if (path === null) drives = await api.listDrives();
      else listing = await api.listDir(path);
    } catch (err) {
      if (token === this.navToken) setStatus(String(err), true);
      return false;
    }
    if (token !== this.navToken) return false;

    const newPath = listing ? listing.path : null;
    if (push && this.loaded && newPath !== this.path) {
      this.back.push(this.path);
      this.fwd = [];
    }
    this.cancelSizes();
    this.loaded = true;
    this.path = newPath;
    this.listing = listing;
    if (path === null) this.drives = drives;
    this.selected.clear();
    this.anchor = null;
    this.focus = opts.highlight ?? null;
    this.sizes.clear();
    this.computeRows();
    this.render();
    if (opts.highlight) this.flash(opts.highlight);
    this.startSizes();
    paneChanged(this);
    return true;
  }

  goBack() {
    if (!this.back.length) return;
    const prev = this.back.pop()!;
    this.fwd.push(this.path);
    this.navigate(prev, { push: false });
  }

  goForward() {
    if (!this.fwd.length) return;
    const next = this.fwd.pop()!;
    this.back.push(this.path);
    this.navigate(next, { push: false });
  }

  up() {
    if (this.path === null) return;
    this.navigate(this.listing?.parent ?? null, { highlight: this.path });
  }

  async refresh() {
    if (this.path) await api.invalidateSizes(this.path);
    const keep = new Set(this.selected);
    const ok = await this.navigate(this.path, { push: false });
    if (ok) {
      for (const p of keep) if (this.rows.some((r) => r.path === p)) this.selected.add(p);
      this.selectionChanged();
    }
  }

  open(entry: Entry) {
    if (!entry.isDir) return;
    if (this.role === "source" && entry.link) revealLinkTarget(entry);
    this.navigate(entry.path);
  }

  flash(path: string) {
    requestAnimationFrame(() => {
      const i = this.rows.findIndex((r) => r.path.toLowerCase() === path.toLowerCase());
      if (i < 0) return;
      const tr = this.els.tbody.querySelector<HTMLElement>(`tr[data-i="${i}"]`);
      if (!tr) return;
      tr.scrollIntoView({ block: "center" });
      tr.classList.add("flash");
      this.focus = this.rows[i].path;
    });
  }

  // ---- rows / sorting
  computeRows() {
    if (!this.listing) {
      this.rows = [];
      return;
    }
    const rows = this.listing.entries.filter((e) => app.showHidden || !e.hidden);
    const { key, dir } = this.sort;
    const sizeOf = (e: Entry) => {
      if (!e.isDir) return e.size ?? 0;
      const s = this.sizes.get(e.path);
      return s && typeof s === "object" ? s.bytes : -1;
    };
    rows.sort((a, b) => {
      if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
      let r = 0;
      if (key === "name") r = collator.compare(a.name, b.name);
      else if (key === "size") r = sizeOf(a) - sizeOf(b);
      else if (key === "modified") r = (a.modified ?? 0) - (b.modified ?? 0);
      else r = collator.compare(typeLabel(a), typeLabel(b));
      if (r === 0) r = collator.compare(a.name, b.name);
      return r * dir;
    });
    this.rows = rows;
  }

  // ---- sizes
  cancelSizes() {
    this.epoch++;
    this.queue = [];
    this.running = 0;
    api.cancelSizes(this.id).catch(() => {});
  }

  startSizes() {
    if (!app.calcSizes || !this.listing) return;
    for (const r of this.rows) {
      if (r.isDir && !r.link && !this.sizes.has(r.path)) {
        this.sizes.set(r.path, "pending");
        this.queue.push(r.path);
      }
    }
    this.pump();
  }

  pump() {
    const epoch = this.epoch;
    while (this.running < 2 && this.queue.length) {
      const p = this.queue.shift()!;
      this.running++;
      api
        .dirSize(p, this.id)
        .then((s) => {
          if (epoch === this.epoch) this.sizes.set(p, s);
        })
        .catch((err) => {
          if (epoch === this.epoch && String(err) !== "cancelled") this.sizes.set(p, "error");
        })
        .finally(() => {
          if (epoch !== this.epoch) return;
          this.running--;
          this.scheduleRender(true);
          this.pump();
        });
    }
  }

  scheduleRender(resort = false) {
    if (this.renderQueued) return;
    this.renderQueued = true;
    requestAnimationFrame(() => {
      this.renderQueued = false;
      if (resort && this.sort.key === "size" && !app.dragging) this.computeRows();
      this.render();
      if (this === panes[0]) updateSelectionInfo();
    });
  }

  // ---- render
  render() {
    const { drives, table, empty, crumbs, fs } = this.els;
    this.renderCrumbs(crumbs);
    (this.el.querySelector('[data-act="back"]') as HTMLButtonElement).disabled = !this.back.length;
    (this.el.querySelector('[data-act="fwd"]') as HTMLButtonElement).disabled = !this.fwd.length;
    (this.el.querySelector('[data-act="up"]') as HTMLButtonElement).disabled = this.path === null;

    if (this.path === null) {
      table.classList.add("hidden");
      empty.classList.add("hidden");
      drives.classList.remove("hidden");
      drives.innerHTML = this.renderDrives();
      fs.textContent = "";
    } else {
      drives.classList.add("hidden");
      table.classList.remove("hidden");
      this.renderTable();
      empty.classList.toggle("hidden", this.rows.length > 0);
      empty.textContent = this.listing?.entries.length ? "Only hidden items here — enable “Hidden items” to see them." : "This folder is empty.";
      fs.textContent = this.listing?.fs ?? "";
    }
    this.renderFoot();
  }

  renderCrumbs(el: HTMLElement) {
    const pc = `<button class="crumb" data-path="" title="This Computer">${app.platform === "windows" ? "This PC" : "Computer"}</button>`;
    if (!this.listing) {
      el.innerHTML = pc;
      // a crumb with empty path means "This PC"
      el.querySelector<HTMLElement>("[data-path]")!.dataset.path = "";
      return;
    }
    const sep = `<span class="crumb-sep">›</span>`;
    el.innerHTML =
      pc +
      this.listing.crumbs.map((c) => `${sep}<button class="crumb" data-path="${esc(c.path)}" title="${esc(c.path)}">${esc(c.name)}</button>`).join("");
    el.scrollLeft = el.scrollWidth;
  }

  renderDrives(): string {
    const cards = this.drives.map((d) => {
      const used = d.total ? (d.total - d.free) / d.total : 0;
      const label = app.platform === "windows" ? `${esc(d.name || (d.removable ? "USB Drive" : "Local Disk"))} <small>(${esc(d.mount.replace(/\\$/, ""))})</small>` : `${esc(d.mount)} <small>${esc(baseName(d.name))}</small>`;
      return `<button class="drive" data-mount="${esc(d.mount)}">
        <span class="drive-ico">${d.removable ? icons.usb() : icons.drive()}</span>
        <span class="drive-main">
          <div class="drive-name">${label}</div>
          <div class="bar"><span class="${used > 0.9 ? "full" : ""}" style="width:${(used * 100).toFixed(1)}%"></span></div>
          <div class="drive-meta"><span>${fmtBytes(d.free)} free of ${fmtBytes(d.total)}</span><span>${esc(d.fs)}${d.kind !== "unknown" ? " · " + d.kind.toUpperCase() : ""}</span></div>
        </span>
      </button>`;
    });
    const home = app.home
      ? `<div class="drives-title">Quick access</div>
         <button class="drive" data-mount="${esc(app.home)}"><span class="drive-ico">${icons.homeBig()}</span>
         <span class="drive-main"><div class="drive-name">Home <small>${esc(app.home)}</small></div><div class="drive-meta"><span>Your user folder</span></div></span></button>`
      : "";
    return `<div class="drives-title">Drives</div>${cards.join("")}${home}`;
  }

  renderTable() {
    const { thead, tbody, selall } = this.els;
    thead.querySelectorAll<HTMLElement>("th[data-sort]").forEach((th) => {
      const on = th.dataset.sort === this.sort.key;
      th.classList.toggle("sorted", on);
      const label = th.textContent!.replace(/[▲▼]/g, "").trim();
      th.innerHTML = `${label}${on ? `<span class="arr">${this.sort.dir === 1 ? "▲" : "▼"}</span>` : ""}`;
    });
    selall.classList.toggle("on", this.rows.length > 0 && this.selected.size === this.rows.length);

    const up = `<tr class="up-row" data-up><td class="c-check"></td><td><div class="name"><span class="ico">${icons.up()}</span><span class="nm">..</span></div></td><td class="c-size"></td><td class="c-mod"></td><td class="c-type"></td></tr>`;
    const html = this.rows.map((e, i) => {
      const lk = linkClass(e);
      const cls = [lk && `lk-${lk}`, this.selected.has(e.path) && "sel", this.focus === e.path && "focus", e.hidden && "dim"].filter(Boolean).join(" ");
      const ico = e.isDir ? (e.link ? icons.folderLink() : icons.folder()) : e.link && e.link.kind !== "hardlink" ? icons.fileLink() : icons.file();
      const tgt = e.link?.target ? `<span class="tgt-arrow">${icons.arrowRight(11)}</span><span class="tgt" title="${esc(e.link.target)}"><bdi>${esc(e.link.target)}</bdi></span>` : "";
      const pill = e.link ? `<span class="pill ${lk}">${esc(typeLabel(e))}</span>` : `<span class="muted">${esc(typeLabel(e))}</span>`;
      return `<tr class="${cls}" data-i="${i}" draggable="${this.role === "source"}">
        <td class="c-check"><span class="cb">${icons.check(12)}</span></td>
        <td><div class="name"><span class="ico ${e.isDir ? "dir" : ""}">${ico}</span><span class="nm" title="${esc(e.name)}">${esc(e.name)}</span>${tgt}</div></td>
        <td class="c-size num">${this.sizeCell(e)}</td>
        <td class="c-mod muted">${e.modified ? dateFmt.format(e.modified) : ""}</td>
        <td class="c-type">${pill}</td>
      </tr>`;
    });
    tbody.innerHTML = up + html.join("");
  }

  sizeCell(e: Entry): string {
    if (!e.isDir) return e.size != null ? fmtBytes(e.size) : "";
    if (e.link) return `<span class="faint">link</span>`;
    const s = this.sizes.get(e.path);
    if (s === "pending") return `<span class="shimmer"></span>`;
    if (s === "error") return `<span class="faint" title="Could not read">—</span>`;
    if (s) {
      const t = `${s.files.toLocaleString()} files, ${s.dirs.toLocaleString()} folders${s.errors ? " — some folders were not accessible" : ""}`;
      return `<span title="${t}">${s.errors ? "≥ " : ""}${fmtBytes(s.bytes)}</span>`;
    }
    return "";
  }

  renderFoot() {
    const f = this.els.foot;
    if (this.path === null) {
      f.innerHTML = `<span><b>${this.drives.length}</b> drives</span>`;
      return;
    }
    const sel = this.selectedEntries();
    let selTxt = "";
    if (sel.length) {
      const bytes = sel.reduce((n, e) => n + (e.isDir ? (typeof this.sizes.get(e.path) === "object" ? (this.sizes.get(e.path) as SizeInfo).bytes : 0) : e.size ?? 0), 0);
      selTxt = `<span><b>${sel.length}</b> selected${bytes ? ` · ${fmtBytes(bytes)}` : ""}</span>`;
    }
    const pending = [...this.sizes.values()].filter((v) => v === "pending").length;
    const l = this.listing!;
    const space = l.free != null && l.total ? `${fmtBytes(l.free)} free of ${fmtBytes(l.total)}` : "";
    f.innerHTML = `<span><b>${this.rows.length}</b> items</span>${selTxt}${pending ? `<span>measuring ${pending} folders…</span>` : ""}<span class="spacer"></span><span>${esc(l.fs)}${space ? " · " + space : ""}</span>`;
  }

  // ---- selection
  selectedEntries(): Entry[] {
    return this.rows.filter((r) => this.selected.has(r.path));
  }

  selectionChanged() {
    this.render();
    if (this.role === "source") updateSelectionInfo();
    paneChanged(this);
  }

  onRowClick(e: MouseEvent) {
    const tr = (e.target as HTMLElement).closest<HTMLElement>("tr");
    if (!tr) return;
    if (tr.dataset.up !== undefined) {
      this.up();
      return;
    }
    const entry = this.rows[Number(tr.dataset.i)];
    if (!entry) return;
    const touch = app.lastPointer === "touch" || app.lastPointer === "pen";
    const onCheck = !!(e.target as HTMLElement).closest(".c-check");

    if (onCheck || (touch && !entry.isDir)) {
      this.toggle(entry.path);
    } else if (touch) {
      this.open(entry);
      return;
    } else if (e.ctrlKey || e.metaKey) {
      this.toggle(entry.path);
    } else if (e.shiftKey && this.anchor) {
      this.selectRange(this.anchor, entry.path);
    } else {
      this.selected = new Set([entry.path]);
      this.anchor = entry.path;
    }
    this.focus = entry.path;
    this.selectionChanged();
    if (this.role === "source" && entry.link?.target && !onCheck) revealLinkTarget(entry);
  }

  toggle(path: string) {
    if (this.selected.has(path)) this.selected.delete(path);
    else this.selected.add(path);
    this.anchor = path;
  }

  selectRange(a: string, b: string) {
    const ia = this.rows.findIndex((r) => r.path === a);
    const ib = this.rows.findIndex((r) => r.path === b);
    if (ia < 0 || ib < 0) return;
    const [lo, hi] = ia < ib ? [ia, ib] : [ib, ia];
    this.selected = new Set(this.rows.slice(lo, hi + 1).map((r) => r.path));
  }

  // ---- keyboard
  moveFocus(delta: number, extend: boolean) {
    if (!this.rows.length) return;
    let i = this.rows.findIndex((r) => r.path === this.focus);
    i = i < 0 ? (delta > 0 ? 0 : this.rows.length - 1) : Math.max(0, Math.min(this.rows.length - 1, i + delta));
    const p = this.rows[i].path;
    this.focus = p;
    if (extend) {
      this.selectRange(this.anchor ?? p, p);
    } else if (this.selected.size <= 1) {
      this.selected = new Set([p]);
      this.anchor = p;
    }
    this.selectionChanged();
    this.els.tbody.querySelector(`tr[data-i="${i}"]`)?.scrollIntoView({ block: "nearest" });
  }

  focusedEntry(): Entry | undefined {
    return this.rows.find((r) => r.path === this.focus);
  }

  // ---- drag & drop
  onDragStart(e: DragEvent) {
    if (this.role !== "source") return;
    const tr = (e.target as HTMLElement).closest<HTMLElement>("tr");
    const entry = tr ? this.rows[Number(tr.dataset.i)] : undefined;
    if (!entry || !e.dataTransfer) return;
    if (!this.selected.has(entry.path)) {
      this.selected = new Set([entry.path]);
      this.anchor = entry.path;
      this.focus = entry.path;
      this.selectionChanged();
    }
    app.dragging = [...this.selected];
    e.dataTransfer.setData("application/x-dirlinker", JSON.stringify(app.dragging));
    e.dataTransfer.effectAllowed = "link";
    const ghost = document.createElement("div");
    ghost.className = "drag-ghost";
    const n = app.dragging.length;
    ghost.innerHTML = `${icons.link(16)} ${n === 1 ? esc(baseName(app.dragging[0])) : `${n} items`}`;
    document.body.appendChild(ghost);
    e.dataTransfer.setDragImage(ghost, -12, -12);
    setTimeout(() => ghost.remove(), 0);
  }

  dropDest(e: DragEvent): { dest: string; mark: HTMLElement } | null {
    const t = e.target as HTMLElement;
    const drive = t.closest<HTMLElement>("[data-mount]");
    if (drive) return { dest: drive.dataset.mount!, mark: drive };
    const tr = t.closest<HTMLElement>("tr[data-i]");
    if (tr) {
      const entry = this.rows[Number(tr.dataset.i)];
      if (entry?.isDir) return { dest: entry.path, mark: tr };
    }
    if (this.path) return { dest: this.path, mark: this.el };
    return null;
  }

  onDragOver(e: DragEvent) {
    if (!app.dragging) return;
    const d = this.dropDest(e);
    if (!d) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "link";
    if (!d.mark.classList.contains(d.mark === this.el ? "drop-ok" : "drop-target")) {
      clearDropMarks();
      d.mark.classList.add(d.mark === this.el ? "drop-ok" : "drop-target");
      if (d.mark !== this.el) this.el.classList.add("drop-ok");
    }
  }

  onDrop(e: DragEvent) {
    if (!app.dragging) return;
    const d = this.dropDest(e);
    if (!d) return;
    e.preventDefault();
    const sources = app.dragging;
    endDrag();
    openLinkDialog(sources, d.dest);
  }
}

function clearDropMarks() {
  document.querySelectorAll(".drop-target, .drop-ok").forEach((el) => el.classList.remove("drop-target", "drop-ok"));
}

function endDrag() {
  app.dragging = null;
  clearDropMarks();
}

// ---------------------------------------------------------------- panes & cross-pane behaviour

const panes = [new Pane(0, $("#pane-0"), "source"), new Pane(1, $("#pane-1"), "dest")];

function setActive(id: number) {
  app.active = id;
  panes.forEach((p) => p.el.classList.toggle("active", p.id === id));
}

/** Clicking a link on the left shows where the real folder lives on the right. */
function revealLinkTarget(entry: Entry) {
  if (!app.followLinks || !entry.link?.target) return;
  if (!entry.link.targetExists) {
    setStatus(`Broken link: ${entry.link.target} does not exist.`, true);
    return;
  }
  const t = entry.link.target;
  panes[1].navigate(parentOf(t) ?? t, { highlight: t });
  setStatus(`${entry.name} is a ${KIND_LABEL[entry.link.kind].toLowerCase()} → ${t}`);
}

function sourcePaths(): string[] {
  return panes[0].path === null ? [] : [...panes[0].selected];
}

/** Destination: the single folder selected on the right, otherwise the right pane's current folder. */
function destPath(): string {
  const r = panes[1];
  if (r.selected.size === 1) {
    const e = r.selectedEntries()[0];
    if (e?.isDir) return e.path;
  }
  return r.path ?? "";
}

function paneChanged(_p: Pane) {
  scheduleAnalysis();
}

function updateSelectionInfo() {
  renderPreview();
}

// ---------------------------------------------------------------- link bar

let analysisTimer = 0;
let analysisSeq = 0;
function scheduleAnalysis() {
  clearTimeout(analysisTimer);
  analysisTimer = window.setTimeout(runAnalysis, 90);
}

async function runAnalysis() {
  const seq = ++analysisSeq;
  const a = await api.linkOptions(sourcePaths(), destPath(), app.mode).catch(() => null);
  if (seq !== analysisSeq || !a) return;
  app.analysis = a;
  const cur = a.options.find((o) => o.kind === app.kind);
  if (!cur || !cur.available || !app.userPickedKind) app.kind = a.recommended ?? (cur?.available ? cur.kind : null);
  renderLinkBar();
}

function chosenOption(): LinkOption | undefined {
  return app.analysis?.options.find((o) => o.kind === app.kind);
}

function renderLinkBar() {
  const a = app.analysis;
  if (!a) return;
  const noSel = sourcePaths().length === 0;

  $("#fs-info").textContent = noSel ? (a.dstFs ? `destination: ${a.dstFs}` : "") : a.srcFs || a.dstFs ?`${a.srcFs || "?"} → ${a.dstFs || "?"}${a.srcFs && a.dstFs ? (a.sameVolume ? " · same drive" : " · different drives") : ""}` : "";

  $("#kinds").innerHTML = a.options
    .map((o) => {
      const off = !noSel && !o.available;
      const cls = ["kind", o.kind === app.kind && !noSel && "on", off && "off"].filter(Boolean).join(" ");
      const badges = `${o.recommended && !noSel ? `<span class="rec" title="Recommended">${icons.star()}</span>` : ""}${o.needsAdmin ? `<span class="adm" title="Needs elevated rights">${icons.shield(12)}</span>` : ""}`;
      return `<button class="${cls}" data-kind="${o.kind}" title="${esc(off ? o.reason ?? "" : o.short)}">
        <span class="k-ico">${kindIcon[o.kind]}</span>
        <span class="k-title">${esc(o.title)}${badges}</span>
        <span class="k-short">${esc(off ? o.reason ?? o.short : o.short)}</span>
      </button>`;
    })
    .join("");

  const modeDesc =
    app.mode === "move"
      ? "Moves the selected items into the destination and leaves a link at the original place — programs keep using the old path."
      : "Leaves the data where it is and creates a link inside the destination that points to it.";
  const opt = chosenOption();
  $("#mode-desc").innerHTML = esc(modeDesc) + (opt?.keepsSource ? `<br><span class="faint">${esc(opt.title)} always keeps the original and creates the new entry in the destination.</span>` : "");
  document.querySelectorAll<HTMLElement>("#mode-seg button").forEach((b) => b.classList.toggle("on", b.dataset.mode === app.mode));

  renderDetail();
  renderPreview();
}

function renderDetail() {
  const a = app.analysis;
  if (!a) return;
  const k = app.detailKind ?? app.kind ?? a.options[0]?.kind;
  const o = a.options.find((x) => x.kind === k);
  const el = $("#kind-detail");
  if (!o) {
    el.innerHTML = "";
    return;
  }
  const noSel = sourcePaths().length === 0;
  el.innerHTML = `<h4><span style="color:var(--${o.kind})">${kindIcon[o.kind]}</span>${esc(o.title)}</h4>
    <p>${esc(o.details)}</p>
    <p class="use"><b>Use it for:</b> ${esc(o.useWhen)}</p>
    ${!noSel && !o.available && o.reason ? `<p class="why">${icons.warn(14)}<span>${esc(o.reason)}</span></p>` : ""}`;
}

function renderPreview() {
  const a = app.analysis;
  const sources = sourcePaths();
  const dest = destPath();
  const btn = $<HTMLButtonElement>("#link-btn");
  const hint = $("#rail-hint");
  const pv = $("#preview");
  const opt = chosenOption();

  if (!sources.length) {
    btn.disabled = true;
    hint.textContent = "select on the left";
    pv.innerHTML = `<div class="faint">Select folders or files in the left pane, open the destination on the right, then drag them over or press Link.</div>`;
    return;
  }
  const blocking = a?.blocking ?? [];
  const ok = !!opt?.available && blocking.length === 0;
  btn.disabled = !ok;
  hint.textContent = ok ? `${sources.length} item${sources.length > 1 ? "s" : ""} → ${baseName(dest)}` : blocking.length ? "see below" : "choose a link type";

  const arrow = icons.arrowRight(13);
  const lines = sources.slice(0, 3).map((s, i) => {
    const t = a?.targets[i] ?? (dest ? `${dest.replace(/[\\/]$/, "")}${app.platform === "windows" ? "\\" : "/"}${baseName(s)}` : "?");
    if (app.mode === "move" && !opt?.keepsSource) return `<div class="pv"><span><b>${esc(s)}</b></span>${arrow}<span>${esc(t)}</span></div>`;
    return `<div class="pv"><span><b>${esc(t)}</b></span>${arrow}<span>${esc(s)}</span></div>`;
  });
  if (sources.length > 3) lines.push(`<div class="pv faint">… and ${sources.length - 3} more</div>`);
  pv.innerHTML = blocking.map((b) => `<div class="pv block">${icons.warn(13)}<span>${esc(b)}</span></div>`).join("") + lines.join("");
}

$("#kinds").addEventListener("click", (e) => {
  const b = (e.target as HTMLElement).closest<HTMLElement>("[data-kind]");
  if (!b) return;
  const k = b.dataset.kind as LinkKind;
  app.detailKind = k;
  const o = app.analysis?.options.find((x) => x.kind === k);
  if (o?.available || sourcePaths().length === 0) {
    app.kind = k;
    app.userPickedKind = true;
  }
  renderLinkBar();
});
$("#kinds").addEventListener("mouseover", (e) => {
  const b = (e.target as HTMLElement).closest<HTMLElement>("[data-kind]");
  if (b && app.detailKind !== b.dataset.kind) {
    app.detailKind = b.dataset.kind as LinkKind;
    renderDetail();
  }
});
$("#kinds").addEventListener("mouseleave", () => {
  app.detailKind = null;
  renderDetail();
});
$("#mode-seg").addEventListener("click", (e) => {
  const m = (e.target as HTMLElement).closest<HTMLElement>("[data-mode]")?.dataset.mode as Mode | undefined;
  if (m && m !== app.mode) {
    app.mode = m;
    renderLinkBar();
    scheduleAnalysis();
  }
});
$("#link-btn").addEventListener("click", () => openLinkDialog(sourcePaths(), destPath()));

// ---------------------------------------------------------------- dialog

const modal = {
  open: false,
  running: false,
  sources: [] as string[],
  dest: "",
  mode: "move" as Mode,
  kind: null as LinkKind | null,
  analysis: null as Analysis | null,
};

const PHASES: Record<string, string> = {
  scanning: "Measuring",
  copying: "Copying",
  verifying: "Verifying copy",
  moving: "Moving",
  linking: "Creating link",
  cloning: "Cloning",
  cleaning: "Removing old copy",
};

async function openLinkDialog(sources: string[], dest: string) {
  if (!sources.length) return setStatus("Select one or more items on the left first.", true);
  if (!dest) return setStatus("Open a destination folder on the right first.", true);
  Object.assign(modal, { open: true, running: false, sources, dest, mode: app.mode, kind: app.userPickedKind ? app.kind : null });
  $("#modal").classList.remove("hidden");
  await refreshDialog();
}

async function refreshDialog() {
  modal.analysis = await api.linkOptions(modal.sources, modal.dest, modal.mode);
  const a = modal.analysis;
  const cur = a.options.find((o) => o.kind === modal.kind && o.available);
  if (!cur) modal.kind = a.recommended;
  renderDialog();
}

function renderDialog() {
  const a = modal.analysis!;
  const opt = a.options.find((o) => o.kind === modal.kind);
  const moving = modal.mode === "move" && !opt?.keepsSource;
  const n = modal.sources.length;
  const arrow = icons.arrowRight(16);
  const items = modal.sources
    .map((s, i) => {
      const t = a.targets[i] ?? "";
      const P = (p: string) => `<span class="pp" title="${esc(p)}"><bdi>${esc(p)}</bdi></span>`;
      if (moving)
        return `<div class="m-item"><div class="p">${P(s)}<small>becomes a ${esc(opt?.title.toLowerCase() ?? "link")}</small></div>${arrow}<div class="p">${P(t)}<small>data moves here</small></div></div>`;
      if (opt?.keepsSource)
        return `<div class="m-item"><div class="p">${P(s)}<small>stays as is</small></div>${arrow}<div class="p">${P(t)}<small>new ${esc(opt.title.toLowerCase())}</small></div></div>`;
      return `<div class="m-item"><div class="p">${P(t)}<small>new ${esc(opt?.title.toLowerCase() ?? "link")}</small></div>${arrow}<div class="p">${P(s)}<small>data stays here</small></div></div>`;
    })
    .join("");
  const chips = a.options
    .map((o) => `<button class="chip ${o.kind === modal.kind ? "on" : ""}" data-kind="${o.kind}" ${o.available ? "" : "disabled"} title="${esc(o.available ? o.short : o.reason ?? "")}">${o.recommended ? icons.star(12) : ""}${esc(o.title)}</button>`)
    .join("");
  const notes = [
    ...a.blocking.map((b) => `<div class="note err">${icons.warn(15)}<span>${esc(b)}</span></div>`),
    ...(moving ? a.warnings : []).map((w) => `<div class="note warn">${icons.warn(15)}<span>${esc(w)}</span></div>`),
    opt ? `<div class="note">${kindIcon[opt.kind]}<span><b>${esc(opt.title)}:</b> ${esc(opt.short)} ${esc(opt.useWhen)}</span></div>` : "",
  ].join("");
  const canStart = !!opt?.available && a.blocking.length === 0;

  $("#modal-box").innerHTML = `
    <div class="m-head"><div class="m-ico">${icons.link(22)}</div><div>
      <h3>${moving ? "Move & link back" : opt?.keepsSource ? `Create ${esc(opt.title.toLowerCase())}` : "Create links"}</h3>
      <p>${n} item${n > 1 ? "s" : ""} → ${esc(modal.dest)}</p></div></div>
    <div class="m-body">
      <div class="seg" id="m-mode"><button data-mode="move" class="${modal.mode === "move" ? "on" : ""}">Move &amp; link back</button><button data-mode="link" class="${modal.mode === "link" ? "on" : ""}">Link only</button></div>
      <div class="m-kinds" id="m-kinds">${chips}</div>
      <div class="m-items">${items}</div>
      ${notes}
    </div>
    <div class="m-foot"><button class="btn" id="m-cancel">Cancel</button><button class="btn primary" id="m-start" ${canStart ? "" : "disabled"}>${moving ? "Move & link" : "Create"}</button></div>`;

  $("#m-cancel").onclick = closeDialog;
  $("#m-start").onclick = startOperation;
  $("#m-mode").onclick = (e) => {
    const m = (e.target as HTMLElement).closest<HTMLElement>("[data-mode]")?.dataset.mode as Mode | undefined;
    if (m && m !== modal.mode) {
      modal.mode = m;
      refreshDialog();
    }
  };
  $("#m-kinds").onclick = (e) => {
    const k = (e.target as HTMLElement).closest<HTMLElement>("[data-kind]")?.dataset.kind as LinkKind | undefined;
    if (k) {
      modal.kind = k;
      renderDialog();
    }
  };
}

function closeDialog() {
  if (modal.running) return;
  modal.open = false;
  $("#modal").classList.add("hidden");
}

async function startOperation() {
  const a = modal.analysis;
  if (!a || !modal.kind || modal.running) return;
  const opt = a.options.find((o) => o.kind === modal.kind);
  if (!opt?.available || a.blocking.length) return;
  modal.running = true;
  // remember the user's choice in the main window too
  app.mode = modal.mode;
  app.kind = modal.kind;
  app.userPickedKind = true;

  $("#modal-box").innerHTML = `
    <div class="m-head"><div class="m-ico">${icons.link(22)}</div><div><h3>Working…</h3><p id="p-item">Starting</p></div></div>
    <div class="m-body"><div class="prog">
      <div class="prog-bar indet" id="p-bar"><span></span></div>
      <div class="prog-meta"><span id="p-phase">Preparing</span><span id="p-bytes"></span></div>
      <div class="prog-file" id="p-file"></div>
    </div></div>
    <div class="m-foot"><span class="faint" style="margin-right:auto">Please keep DirLinker open until this finishes.</span></div>`;

  const unlisten = await api.onProgress((p: Progress) => {
    $("#p-item").textContent = `${p.itemCount > 1 ? `(${p.itemIndex + 1}/${p.itemCount}) ` : ""}${p.item}`;
    $("#p-phase").textContent = PHASES[p.phase] ?? p.phase;
    const det = p.bytesTotal > 0 && (p.phase === "copying" || p.phase === "cloning");
    const bar = $("#p-bar");
    bar.classList.toggle("indet", !det);
    ($("span", bar) as HTMLElement).style.width = det ? `${((p.bytesDone / p.bytesTotal) * 100).toFixed(1)}%` : "";
    $("#p-bytes").textContent = p.bytesTotal ? `${fmtBytes(p.bytesDone)} / ${fmtBytes(p.bytesTotal)}` : "";
    $("#p-file").textContent = p.file ?? "";
  });

  let results: ItemResult[] = [];
  try {
    results = await api.execute({ sources: modal.sources, dest: modal.dest, kind: modal.kind, mode: modal.mode });
  } catch (err) {
    results = [{ source: "", target: "", ok: false, message: String(err) }];
  }
  unlisten();
  modal.running = false;
  showResults(results);
  panes.forEach((p) => p.selected.clear());
  await Promise.all(panes.map((p) => p.refresh()));
}

function showResults(results: ItemResult[]) {
  const ok = results.filter((r) => r.ok).length;
  const bad = results.length - ok;
  const list = results
    .map(
      (r) => `<div class="res ${r.ok ? "ok" : "bad"}"><span class="ri">${r.ok ? icons.check(12) : icons.x(12)}</span>
      <div class="rt"><b>${esc(baseName(r.source || r.target || "Operation"))}</b><span>${esc(r.message)}</span></div></div>`,
    )
    .join("");
  $("#modal-box").innerHTML = `
    <div class="m-head"><div class="m-ico" style="background:${bad ? "var(--danger)" : "var(--ok)"}">${bad ? icons.warn(22) : icons.check(22)}</div>
      <div><h3>${bad ? (ok ? "Partly done" : "Failed") : "Done"}</h3><p>${ok} succeeded${bad ? `, ${bad} failed` : ""}</p></div></div>
    <div class="m-body"><div class="m-items">${list}</div></div>
    <div class="m-foot">${ok ? `<button class="btn" id="m-reveal">Show in file manager</button>` : ""}<button class="btn primary" id="m-close">Close</button></div>`;
  $("#m-close").onclick = closeDialog;
  const first = results.find((r) => r.ok);
  if (first) $("#m-reveal").onclick = () => api.reveal(first.target).catch((e) => setStatus(String(e), true));
  setStatus(bad ? `${bad} operation(s) failed.` : `${ok} item(s) linked successfully.`, bad > 0);
}

// ---------------------------------------------------------------- global events

document.addEventListener("keydown", (e) => {
  if (modal.open) {
    if (e.key === "Escape") closeDialog();
    else if (e.key === "Enter" && !modal.running) $<HTMLButtonElement>("#m-start")?.click();
    return;
  }
  if ((e.target as HTMLElement).tagName === "INPUT") return;
  const p = panes[app.active];
  const k = e.key;
  if (k === "Tab") {
    e.preventDefault();
    setActive(1 - app.active);
  } else if (k === "ArrowDown" || k === "j") {
    e.preventDefault();
    p.moveFocus(1, e.shiftKey);
  } else if (k === "ArrowUp" || k === "k") {
    e.preventDefault();
    p.moveFocus(-1, e.shiftKey);
  } else if (k === "PageDown") p.moveFocus(15, e.shiftKey);
  else if (k === "PageUp") p.moveFocus(-15, e.shiftKey);
  else if (k === "Home") p.moveFocus(-1e9, e.shiftKey);
  else if (k === "End") p.moveFocus(1e9, e.shiftKey);
  else if (k === "Enter" && (e.ctrlKey || e.metaKey)) openLinkDialog(sourcePaths(), destPath());
  else if (k === "Enter" || k === "l" || (k === "ArrowRight" && !e.altKey)) {
    const f = p.focusedEntry();
    if (f) p.open(f);
  } else if (k === "Backspace" || k === "h" || (k === "ArrowLeft" && !e.altKey)) {
    e.preventDefault();
    p.up();
  } else if (k === "ArrowLeft" && e.altKey) p.goBack();
  else if (k === "ArrowRight" && e.altKey) p.goForward();
  else if (k === " ") {
    e.preventDefault();
    const f = p.focusedEntry();
    if (f) {
      p.toggle(f.path);
      p.selectionChanged();
      p.moveFocus(1, false);
    }
  } else if (k.toLowerCase() === "a" && (e.ctrlKey || e.metaKey)) {
    e.preventDefault();
    p.selected = new Set(p.rows.map((r) => r.path));
    p.selectionChanged();
  } else if (k === "F5") {
    e.preventDefault();
    p.refresh();
  } else if (k === "Escape") {
    p.selected.clear();
    p.selectionChanged();
  }
});

document.addEventListener("contextmenu", (e) => {
  if ((e.target as HTMLElement).tagName !== "INPUT") e.preventDefault();
});

$<HTMLInputElement>("#opt-sizes").addEventListener("change", (e) => {
  app.calcSizes = (e.target as HTMLInputElement).checked;
  panes.forEach((p) => {
    if (app.calcSizes) p.startSizes();
    else {
      p.cancelSizes();
      p.sizes.clear();
    }
    p.render();
  });
});
$<HTMLInputElement>("#opt-hidden").addEventListener("change", (e) => {
  app.showHidden = (e.target as HTMLInputElement).checked;
  panes.forEach((p) => {
    p.computeRows();
    p.render();
    p.startSizes();
  });
});
$<HTMLInputElement>("#opt-follow").addEventListener("change", (e) => {
  app.followLinks = (e.target as HTMLInputElement).checked;
});

// ---------------------------------------------------------------- start

(async function init() {
  $("#brand-logo").innerHTML = icons.link(22);
  $("#link-btn-ico").innerHTML = icons.link(26);
  try {
    app.platform = await api.platform();
    app.home = await api.homeDir();
  } catch {
    /* running outside Tauri (plain browser) */
  }
  setActive(0);
  await Promise.all(panes.map((p) => p.navigate(null)));
  scheduleAnalysis();
})();
