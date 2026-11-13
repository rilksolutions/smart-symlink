import {
  api,
  type Analysis,
  type DriveInfo,
  type Entry,
  type ItemResult,
  type LinkKind,
  type LinkOption,
  type Listing,
  type Mode,
  type Progress,
  type SavedLinkView,
  type SizeInfo,
  type UnlinkMode,
} from "./api";
import { icons, kindIcon } from "./icons";

// ================================================================ helpers

const $ = <T extends HTMLElement = HTMLElement>(sel: string, root: ParentNode = document) => root.querySelector(sel) as T;
const ESC: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
const esc = (s: string) => s.replace(/[&<>"']/g, (c) => ESC[c]);
const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });
const dateFmt = new Intl.DateTimeFormat(undefined, { dateStyle: "short", timeStyle: "short" });
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

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

const STATUS_LABEL = { ok: "OK", broken: "Target missing", missing: "Link deleted", changed: "Changed" };

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

// ================================================================ app state

const LINKS = "::links"; // pseudo-path of the "Saved links" view
type View = "drives" | "dir" | "links";
type Theme = "system" | "light" | "dark";

const app = {
  platform: "windows",
  home: null as string | null,
  showHidden: false,
  showDot: false,
  calcSizes: true,
  followLinks: true,
  mode: "move" as Mode,
  kind: null as LinkKind | null,
  userPickedKind: false,
  pickedKind: null as LinkKind | null,
  detailKind: null as LinkKind | null,
  analysis: null as Analysis | null,
  active: 0,
  dragging: null as string[] | null,
  lastPointer: "mouse",
  favorites: [] as string[],
  saved: [] as SavedLinkView[],
  /** key(target) -> link path, for saved links that are intact */
  targets: new Map<string, string>(),
  theme: "system" as Theme,
  verifyContents: false,
  privNote: null as string | null,
};

const isWin = () => app.platform === "windows";
/** Comparable form of a path. */
const key = (p: string) => (isWin() ? p.toLowerCase() : p).replace(/(?<=[^\\/:])[\\/]+$/, "");
const isFav = (p: string) => app.favorites.some((f) => key(f) === key(p));
const fmName = () => (isWin() ? "Explorer" : app.platform === "macos" ? "Finder" : "file manager");

type SortKey = "name" | "size" | "modified" | "type";
type SizeState = SizeInfo | "pending" | "error";

async function reloadStore() {
  try {
    [app.favorites, app.saved] = await Promise.all([api.favorites(), api.savedLinks()]);
  } catch {
    return;
  }
  app.targets = new Map(app.saved.filter((s) => s.status === "ok").map((s) => [key(s.saved.target), s.saved.link]));
  $("#saved-count").textContent = String(app.saved.length);
}

// ================================================================ pane

class Pane {
  view: View = "drives";
  path: string | null = null;
  listing: Listing | null = null;
  drives: DriveInfo[] = [];
  links = new Map<string, SavedLinkView>();
  rows: Entry[] = [];
  selected = new Set<string>();
  anchor: string | null = null;
  focus: string | null = null;
  driveFocus = 0;
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
  lastClick: { path: string; t: number } | null = null;
  els: Record<string, HTMLElement> = {};

  constructor(public id: number, public el: HTMLElement, public role: "source" | "dest") {
    this.build();
  }

  get isSource() {
    return this.role === "source";
  }
  /** History/navigation token for the current location. */
  get loc(): string | null {
    return this.view === "links" ? LINKS : this.path;
  }

  // ---------------------------------------------------------- DOM
  build() {
    const src = this.isSource;
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
          <div class="addr" title="Click to type a path (Ctrl+L)"><div class="crumbs"></div><input class="hidden" spellcheck="false" /></div>
          <button class="tb" data-act="reveal" title="Show in ${fmName()}">${icons.reveal()}</button>
          <button class="tb" data-act="refresh" title="Refresh (F5)">${icons.refresh()}</button>
        </div>
      </div>
      <div class="pane-body-wrap">
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
        ${src ? "" : `<button class="fab hidden" data-act="newfolder" title="New folder (Ctrl+Shift+N)">${icons.folderPlus()}</button>`}
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

    this.el.addEventListener("click", (e) => {
      const act = (e.target as HTMLElement).closest<HTMLElement>("[data-act]")?.dataset.act;
      if (act === "back") this.goBack();
      else if (act === "fwd") this.goForward();
      else if (act === "up") this.up();
      else if (act === "pc") this.navigate(null);
      else if (act === "refresh") this.refresh();
      else if (act === "newfolder") newFolder(this);
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
        if (!this.isSource) return;
        const all = this.selected.size === this.rows.length && this.rows.length > 0;
        this.selected = all ? new Set() : new Set(this.rows.map((r) => r.path));
        this.selectionChanged();
        return;
      }
      const k = th.dataset.sort as SortKey | undefined;
      if (!k) return;
      if (this.sort.key === k) this.sort.dir = this.sort.dir === 1 ? -1 : 1;
      else this.sort = { key: k, dir: k === "name" || k === "type" ? 1 : -1 };
      this.computeRows();
      this.render();
    });

    tbody.addEventListener("click", (e) => this.onRowClick(e as MouseEvent));

    drives.addEventListener("click", (e) => {
      const t = e.target as HTMLElement;
      const unfav = t.closest<HTMLElement>("[data-unfav]");
      if (unfav) {
        e.stopPropagation();
        toggleFavorite(unfav.dataset.unfav!, false);
        return;
      }
      const d = t.closest<HTMLElement>("[data-mount]");
      if (!d) return;
      const target = d.dataset.mount!;
      if (target === LINKS && !this.isSource) panes[0].navigate(LINKS);
      else this.navigate(target);
    });

    body.addEventListener("contextmenu", (e) => this.onContextMenu(e));

    // drag (source)
    tbody.addEventListener("dragstart", (e) => this.onDragStart(e));
    tbody.addEventListener("dragend", () => endDrag());

    // drop (destination)
    if (!this.isSource) {
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

  // ---------------------------------------------------------- navigation
  async navigate(target: string | null, opts: { push?: boolean; highlight?: string | null; keepSelection?: boolean } = {}) {
    const push = opts.push ?? true;
    const token = ++this.navToken;
    let listing: Listing | null = null;
    let drives: DriveInfo[] = [];
    let links: SavedLinkView[] = [];
    const view: View = target === null ? "drives" : target === LINKS ? "links" : "dir";
    try {
      if (view === "drives") drives = await api.listDrives();
      else if (view === "links") {
        await reloadStore();
        links = app.saved;
      } else listing = await api.listDir(target!);
    } catch (err) {
      if (token === this.navToken) setStatus(String(err), true);
      return false;
    }
    if (token !== this.navToken) return false;

    const newLoc = view === "links" ? LINKS : listing ? listing.path : null;
    if (push && this.loaded && newLoc !== this.loc) {
      this.back.push(this.loc);
      this.fwd = [];
    }
    this.cancelSizes();
    this.loaded = true;
    this.view = view;
    this.path = listing ? listing.path : null;
    this.listing = listing;
    if (view === "drives") {
      this.drives = drives;
      this.driveFocus = 0;
    }
    if (view === "links") this.links = new Map(links.map((l) => [l.saved.link, l]));
    if (!opts.keepSelection) {
      this.selected.clear();
      this.anchor = null;
    }
    this.focus = opts.highlight ?? null;
    this.sizes.clear();
    this.el.classList.toggle("links-view", view === "links");
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
    this.fwd.push(this.loc);
    this.navigate(prev, { push: false });
  }

  goForward() {
    if (!this.fwd.length) return;
    const next = this.fwd.pop()!;
    this.back.push(this.loc);
    this.navigate(next, { push: false });
  }

  up() {
    if (this.view === "drives") return;
    if (this.view === "links") return void this.navigate(null);
    this.navigate(this.listing?.parent ?? null, { highlight: this.path });
  }

  async refresh() {
    if (this.path) await api.invalidateSizes(this.path);
    const keep = new Set(this.selected);
    const focus = this.focus;
    const ok = await this.navigate(this.loc, { push: false, highlight: null });
    if (ok) {
      for (const p of keep) if (this.rows.some((r) => r.path === p)) this.selected.add(p);
      this.focus = focus;
      this.selectionChanged();
    }
  }

  open(entry: Entry) {
    if (this.view === "links") {
      // go to the folder that contains the link, with the link highlighted
      const par = parentOf(entry.path);
      if (par) this.navigate(par, { highlight: entry.path });
      return;
    }
    if (!entry.isDir) return;
    if (this.isSource && entry.link) revealLinkTarget(entry);
    this.navigate(entry.path);
  }

  flash(path: string) {
    requestAnimationFrame(() => {
      const i = this.rows.findIndex((r) => key(r.path) === key(path));
      if (i < 0) return;
      const tr = this.els.tbody.querySelector<HTMLElement>(`tr[data-i="${i}"]`);
      if (!tr) return;
      tr.scrollIntoView({ block: "center" });
      tr.classList.add("flash");
      this.focus = this.rows[i].path;
      tr.classList.add("focus");
    });
  }

  // ---------------------------------------------------------- rows / sorting
  computeRows() {
    let rows: Entry[];
    if (this.view === "links") {
      rows = [...this.links.values()].map((v) => {
        const s = v.saved;
        const base: Entry = v.entry ?? { name: baseName(s.link), path: s.link, isDir: true, size: null, modified: null, hidden: false, dot: false, link: null };
        return {
          ...base,
          name: baseName(s.link),
          path: s.link,
          modified: s.created,
          link: { kind: s.kind, target: s.target, targetExists: v.status !== "broken", linkCount: base.link?.linkCount ?? null },
        };
      });
    } else if (this.listing) {
      rows = this.listing.entries.filter((e) => (app.showHidden || !e.hidden) && (app.showDot || !e.dot));
    } else {
      this.rows = [];
      return;
    }
    const { key: k, dir } = this.sort;
    const statusOrder = (e: Entry) => ["ok", "changed", "broken", "missing"].indexOf(this.links.get(e.path)?.status ?? "ok");
    const sizeOf = (e: Entry) => {
      if (this.view === "links") return -statusOrder(e);
      if (!e.isDir) return e.size ?? 0;
      const s = this.sizes.get(e.path);
      return s && typeof s === "object" ? s.bytes : -1;
    };
    rows.sort((a, b) => {
      if (this.view === "dir" && a.isDir !== b.isDir) return a.isDir ? -1 : 1;
      let r = 0;
      if (k === "name") r = collator.compare(this.view === "links" ? a.path : a.name, this.view === "links" ? b.path : b.name);
      else if (k === "size") r = sizeOf(a) - sizeOf(b);
      else if (k === "modified") r = (a.modified ?? 0) - (b.modified ?? 0);
      else r = collator.compare(typeLabel(a), typeLabel(b));
      if (r === 0) r = collator.compare(a.name, b.name);
      return r * dir;
    });
    this.rows = rows;
  }

  // ---------------------------------------------------------- sizes
  cancelSizes() {
    this.epoch++;
    this.queue = [];
    this.running = 0;
    api.cancelSizes(this.id).catch(() => {});
  }

  startSizes() {
    if (!app.calcSizes || this.view !== "dir") return;
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
          this.scheduleSizePaint();
          this.pump();
        });
    }
  }

  /** Size results only touch the size cells, unless the list is sorted by size. */
  scheduleSizePaint() {
    if (this.renderQueued) return;
    this.renderQueued = true;
    requestAnimationFrame(() => {
      this.renderQueued = false;
      if (this.sort.key === "size" && !app.dragging) {
        this.computeRows();
        this.render();
      } else {
        this.els.tbody.querySelectorAll<HTMLElement>("tr[data-i]").forEach((tr) => {
          const e = this.rows[Number(tr.dataset.i)];
          if (e?.isDir) tr.querySelector(".c-size")!.innerHTML = this.sizeCell(e);
        });
        this.renderFoot();
      }
      if (this.isSource) renderPreview();
    });
  }

  // ---------------------------------------------------------- render
  render() {
    const { drives, table, empty, crumbs, fs, thead } = this.els;
    this.renderCrumbs(crumbs);
    (this.el.querySelector('[data-act="back"]') as HTMLButtonElement).disabled = !this.back.length;
    (this.el.querySelector('[data-act="fwd"]') as HTMLButtonElement).disabled = !this.fwd.length;
    (this.el.querySelector('[data-act="up"]') as HTMLButtonElement).disabled = this.view === "drives";
    this.el.querySelector(".fab")?.classList.toggle("hidden", this.view !== "dir");

    if (this.view === "drives") {
      table.classList.add("hidden");
      empty.classList.add("hidden");
      drives.classList.remove("hidden");
      drives.innerHTML = this.renderDrives();
      this.paintDriveFocus(false);
      fs.textContent = "";
    } else {
      drives.classList.add("hidden");
      table.classList.remove("hidden");
      const labels = this.view === "links" ? ["Link → target", "Status", "Created", "Type"] : ["Name", "Size", "Modified", "Type"];
      thead.querySelectorAll<HTMLElement>("th[data-sort]").forEach((th, i) => (th.dataset.label = labels[i]));
      this.renderTable();
      empty.classList.toggle("hidden", this.rows.length > 0);
      empty.textContent =
        this.view === "links"
          ? "No saved links yet. Every link you create with Smart Symlink shows up here."
          : this.listing?.entries.length
            ? "Only hidden items here — turn on “Hidden” or “.dotfiles” in the header to see them."
            : "This folder is empty.";
      fs.textContent = this.view === "links" ? "" : (this.listing?.fs ?? "");
    }
    this.renderFoot();
  }

  renderCrumbs(el: HTMLElement) {
    const pc = `<button class="crumb" data-path="" title="This Computer">${isWin() ? "This PC" : "Computer"}</button>`;
    const sep = `<span class="crumb-sep">›</span>`;
    if (this.view === "links") {
      el.innerHTML = `${pc}${sep}<button class="crumb" data-path="${LINKS}">Saved links</button>`;
      return;
    }
    if (!this.listing) {
      el.innerHTML = pc;
      return;
    }
    el.innerHTML =
      pc + this.listing.crumbs.map((c) => `${sep}<button class="crumb" data-path="${esc(c.path)}" title="${esc(c.path)}">${esc(c.name)}</button>`).join("");
    el.scrollLeft = el.scrollWidth;
  }

  renderDrives(): string {
    const cards = this.drives.map((d) => {
      const used = d.total ? (d.total - d.free) / d.total : 0;
      const label = isWin()
        ? `${esc(d.name || (d.removable ? "USB Drive" : "Local Disk"))} <small>(${esc(d.mount.replace(/\\$/, ""))})</small>`
        : `${esc(d.mount)} <small>${esc(baseName(d.name))}</small>`;
      return `<button class="drive" data-mount="${esc(d.mount)}">
        <span class="drive-ico">${d.removable ? icons.usb() : icons.drive()}</span>
        <span class="drive-main">
          <div class="drive-name">${label}</div>
          <div class="bar"><span class="${used > 0.9 ? "full" : ""}" style="width:${(used * 100).toFixed(1)}%"></span></div>
          <div class="drive-meta"><span>${fmtBytes(d.free)} free of ${fmtBytes(d.total)}</span><span>${esc(d.fs)}${d.kind !== "unknown" ? " · " + d.kind.toUpperCase() : ""}</span></div>
        </span>
      </button>`;
    });
    const quick: string[] = [];
    if (app.home)
      quick.push(`<button class="drive" data-mount="${esc(app.home)}"><span class="drive-ico">${icons.homeBig()}</span>
        <span class="drive-main"><div class="drive-name">Home</div><div class="drive-meta"><span>${esc(app.home)}</span></div></span></button>`);
    for (const f of app.favorites)
      quick.push(`<button class="drive" data-mount="${esc(f)}" data-fav="1"><span class="drive-ico fav-ico">${icons.folder()}</span>
        <span class="drive-main"><div class="drive-name">${esc(baseName(f))}</div><div class="drive-meta"><span title="${esc(f)}">${esc(f)}</span></div></span>
        <span class="unfav" data-unfav="${esc(f)}" title="Remove from Quick Access">${icons.star(14)}</span></button>`);
    if (this.isSource)
      quick.push(`<button class="drive" data-mount="${LINKS}"><span class="drive-ico links-ico">${icons.link(34)}</span>
        <span class="drive-main"><div class="drive-name">Saved links <small>${app.saved.length}</small></div><div class="drive-meta"><span>Links created with Smart Symlink</span></div></span></button>`);
    const tip = app.favorites.length ? "" : `<div class="drives-title faint" style="font-weight:400">Tip: click the ☆ next to a folder (or right-click it) to pin it here.</div>`;
    return `<div class="drives-title">Drives</div>${cards.join("")}<div class="drives-title">Quick access</div>${quick.join("")}${tip}`;
  }

  driveCards(): HTMLElement[] {
    return [...this.els.drives.querySelectorAll<HTMLElement>(".drive")];
  }

  paintDriveFocus(scroll = true) {
    const cards = this.driveCards();
    this.driveFocus = Math.max(0, Math.min(cards.length - 1, this.driveFocus));
    cards.forEach((c, i) => c.classList.toggle("focus", i === this.driveFocus && app.active === this.id));
    if (scroll) cards[this.driveFocus]?.scrollIntoView({ block: "nearest" });
  }

  /** Arrow / hjkl movement between drive cards, by on-screen position. */
  moveDriveFocus(dx: number, dy: number) {
    const cards = this.driveCards();
    if (!cards.length) return;
    const cur = cards[this.driveFocus]?.getBoundingClientRect();
    if (!cur) return;
    const cx = cur.left + cur.width / 2;
    const cy = cur.top + cur.height / 2;
    let best = -1;
    let bestScore = Infinity;
    cards.forEach((c, i) => {
      if (i === this.driveFocus) return;
      const r = c.getBoundingClientRect();
      const x = r.left + r.width / 2 - cx;
      const y = r.top + r.height / 2 - cy;
      const along = dx ? x * dx : y * dy; // distance in the wanted direction
      const across = dx ? Math.abs(y) : Math.abs(x);
      if (along <= 4) return;
      const score = along + across * 3;
      if (score < bestScore) {
        bestScore = score;
        best = i;
      }
    });
    if (best >= 0) {
      this.driveFocus = best;
      this.paintDriveFocus();
    }
  }

  renderTable() {
    const { thead, tbody } = this.els;
    thead.querySelectorAll<HTMLElement>("th[data-sort]").forEach((th) => {
      const on = th.dataset.sort === this.sort.key;
      th.classList.toggle("sorted", on);
      th.innerHTML = `${th.dataset.label}${on ? `<span class="arr">${this.sort.dir === 1 ? "▲" : "▼"}</span>` : ""}`;
    });

    const upName = this.view === "links" || !this.listing?.parent ? (isWin() ? "This PC" : "Computer") : baseName(this.listing.parent);
    const up = `<tr class="up-row" data-up title="Go up to ${esc(upName)} (Backspace)"><td class="c-check"></td><td colspan="4"><div class="name"><span class="ico">${icons.up()}</span><span class="nm">..</span><span class="up-to">Up to ${esc(upName)}</span></div></td></tr>`;
    const links = this.view === "links";
    const html = this.rows.map((e, i) => {
      const lk = linkClass(e);
      const ico = e.isDir ? (e.link ? icons.folderLink() : icons.folder()) : e.link && e.link.kind !== "hardlink" ? icons.fileLink() : icons.file();
      const pill = e.link ? `<span class="pill ${lk}">${esc(links ? KIND_LABEL[e.link.kind] : typeLabel(e))}</span>` : `<span class="muted">${esc(typeLabel(e))}</span>`;
      let name: string;
      let size: string;
      if (links) {
        const st = this.links.get(e.path)!.status;
        name = `<div class="name"><span class="ico ${e.isDir ? "dir" : ""}">${ico}</span><div class="lk-path"><span class="a" title="${esc(e.path)}"><bdi>${esc(e.path)}</bdi></span><span class="b" title="${esc(e.link!.target ?? "")}"><bdi>→ ${esc(e.link!.target ?? "")}</bdi></span></div></div>`;
        size = `<span class="st ${st}">${STATUS_LABEL[st]}</span>`;
      } else {
        const tgt = e.link?.target ? `<span class="tgt-arrow">${icons.arrowRight(11)}</span><span class="tgt" title="${esc(e.link.target)}"><bdi>${esc(e.link.target)}</bdi></span>` : "";
        const linkedFrom = app.targets.get(key(e.path));
        const badge = linkedFrom ? `<span class="tbadge" title="Linked from ${esc(linkedFrom)}">${icons.target(10)} link target</span>` : "";
        const fav = e.isDir ? `<button class="fav ${isFav(e.path) ? "on" : ""}" data-fav title="${isFav(e.path) ? "Remove from" : "Add to"} Quick Access">${isFav(e.path) ? icons.star(13) : icons.starOutline(13)}</button>` : "";
        name = `<div class="name"><span class="ico ${e.isDir ? "dir" : ""}">${ico}</span><span class="nm" title="${esc(e.name)}">${esc(e.name)}</span>${badge}${tgt}${fav}</div>`;
        size = this.sizeCell(e);
      }
      return `<tr class="${lk ? `lk-${lk}` : ""} ${e.hidden || e.dot ? "dim" : ""}" data-i="${i}" draggable="${this.isSource && !links}">
        <td class="c-check"><span class="cb">${icons.check(12)}</span></td>
        <td>${name}</td>
        <td class="c-size num">${size}</td>
        <td class="c-mod muted">${e.modified ? dateFmt.format(e.modified) : ""}</td>
        <td class="c-type">${pill}</td>
      </tr>`;
    });
    tbody.innerHTML = (links ? "" : up) + html.join("");
    this.paintSelection();
    if (!this.isSource) this.paintMarks();
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
    if (this.view === "drives") {
      f.innerHTML = `<span><b>${this.drives.length}</b> drives</span><span class="spacer"></span><span>←↑↓→ / hjkl to move · Enter to open</span>`;
      return;
    }
    const sel = this.selectedEntries();
    let selTxt = "";
    if (sel.length) {
      const bytes = sel.reduce((n, e) => {
        const s = this.sizes.get(e.path);
        return n + (e.isDir ? (s && typeof s === "object" ? s.bytes : 0) : (e.size ?? 0));
      }, 0);
      selTxt = `<span><b>${sel.length}</b> selected${bytes ? ` · ${fmtBytes(bytes)}` : ""}</span>`;
    }
    if (this.view === "links") {
      const all = [...this.links.values()];
      const bad = all.filter((l) => l.status !== "ok").length;
      f.innerHTML = `<span><b>${all.length}</b> saved links</span>${bad ? `<span>${bad} need attention</span>` : ""}${selTxt}<span class="spacer"></span><span>Click to see the target · double-click to open the link's folder</span>`;
      return;
    }
    const pending = [...this.sizes.values()].filter((v) => v === "pending").length;
    const l = this.listing!;
    const space = l.free != null && l.total ? `${fmtBytes(l.free)} free of ${fmtBytes(l.total)}` : "";
    f.innerHTML = `<span><b>${this.rows.length}</b> items</span>${selTxt}${pending ? `<span>measuring ${pending} folders…</span>` : ""}<span class="spacer"></span><span>${esc(l.fs)}${space ? " · " + space : ""}</span>`;
  }

  /** Update selection / focus classes in place (no re-render, so clicks stay on the same elements). */
  paintSelection() {
    this.els.tbody.querySelectorAll<HTMLElement>("tr[data-i]").forEach((tr) => {
      const e = this.rows[Number(tr.dataset.i)];
      tr.classList.toggle("sel", this.selected.has(e.path));
      tr.classList.toggle("focus", this.focus === e.path);
    });
    this.els.selall.classList.toggle("on", this.rows.length > 0 && this.selected.size === this.rows.length);
    this.renderFoot();
  }

  /** Destination: mark the real folders behind the links selected on the left. */
  paintMarks() {
    const targets = selectedLinkTargets();
    this.els.tbody.querySelectorAll<HTMLElement>("tr[data-i]").forEach((tr) => {
      const e = this.rows[Number(tr.dataset.i)];
      tr.classList.toggle("is-target", targets.has(key(e.path)));
    });
  }

  // ---------------------------------------------------------- selection
  selectedEntries(): Entry[] {
    return this.rows.filter((r) => this.selected.has(r.path));
  }

  selectionChanged() {
    this.paintSelection();
    if (this.isSource) {
      panes[1].paintMarks();
      renderPreview();
    }
    paneChanged(this);
  }

  isDouble(path: string) {
    const now = performance.now();
    const dbl = !!this.lastClick && this.lastClick.path === path && now - this.lastClick.t < 450;
    this.lastClick = dbl ? null : { path, t: now };
    return dbl;
  }

  onRowClick(e: MouseEvent) {
    const t = e.target as HTMLElement;
    const tr = t.closest<HTMLElement>("tr");
    if (!tr) return;
    if (tr.dataset.up !== undefined) return void this.up();
    const entry = this.rows[Number(tr.dataset.i)];
    if (!entry) return;
    if (t.closest("[data-fav]")) {
      e.stopPropagation();
      toggleFavorite(entry.path, !isFav(entry.path));
      return;
    }
    const touch = app.lastPointer === "touch" || app.lastPointer === "pen";
    const onCheck = !!t.closest(".c-check");

    // destination: no selection — single click focuses, double click / tap opens
    if (!this.isSource) {
      this.focus = entry.path;
      this.paintSelection();
      if (touch || this.isDouble(entry.path)) this.open(entry);
      return;
    }

    if (!onCheck && !touch && this.isDouble(entry.path)) return void this.open(entry);
    if (onCheck || (touch && !entry.isDir) || (touch && this.view === "links")) {
      this.toggle(entry.path);
    } else if (touch) {
      return void this.open(entry);
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
    if (entry.link?.target && !onCheck && this.selected.has(entry.path)) revealLinkTarget(entry);
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

  // ---------------------------------------------------------- keyboard
  moveFocus(delta: number, extend: boolean) {
    if (!this.rows.length) return;
    let i = this.rows.findIndex((r) => r.path === this.focus);
    i = i < 0 ? (delta > 0 ? 0 : this.rows.length - 1) : Math.max(0, Math.min(this.rows.length - 1, i + delta));
    const entry = this.rows[i];
    this.focus = entry.path;
    if (this.isSource) {
      if (extend) this.selectRange(this.anchor ?? entry.path, entry.path);
      else if (this.selected.size <= 1) {
        this.selected = new Set([entry.path]);
        this.anchor = entry.path;
      }
      this.selectionChanged();
      if (entry.link?.target && this.selected.size === 1) revealLinkTarget(entry);
    } else this.paintSelection();
    this.els.tbody.querySelector(`tr[data-i="${i}"]`)?.scrollIntoView({ block: "nearest" });
  }

  focusedEntry(): Entry | undefined {
    return this.rows.find((r) => r.path === this.focus);
  }

  /** Items an action applies to: the selection (source) or the focused row (destination). */
  actionItems(): Entry[] {
    if (this.isSource && this.selected.size) return this.selectedEntries();
    const f = this.focusedEntry();
    return f ? [f] : [];
  }

  // ---------------------------------------------------------- context menu
  onContextMenu(e: MouseEvent) {
    e.preventDefault();
    const t = e.target as HTMLElement;
    const card = t.closest<HTMLElement>("[data-mount]");
    if (card) return void driveMenu(this, card, e);
    const tr = t.closest<HTMLElement>("tr[data-i]");
    if (tr) {
      const entry = this.rows[Number(tr.dataset.i)];
      if (this.isSource) {
        if (!this.selected.has(entry.path)) {
          this.selected = new Set([entry.path]);
          this.anchor = entry.path;
        }
        this.focus = entry.path;
        this.selectionChanged();
      } else {
        this.focus = entry.path;
        this.paintSelection();
      }
      itemMenu(this, this.isSource ? this.selectedEntries() : [entry], e);
    } else if (this.view === "dir") folderMenu(this, e);
  }

  // ---------------------------------------------------------- drag & drop
  onDragStart(e: DragEvent) {
    if (!this.isSource || this.view !== "dir") return;
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
    e.dataTransfer.setData("application/x-smart-symlink", JSON.stringify(app.dragging));
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
    if (drive && drive.dataset.mount !== LINKS) return { dest: drive.dataset.mount!, mark: drive };
    const tr = t.closest<HTMLElement>("tr[data-i]");
    if (tr && this.view === "dir") {
      const entry = this.rows[Number(tr.dataset.i)];
      if (entry?.isDir) return { dest: entry.path, mark: tr };
    }
    if (this.view === "dir" && this.path) return { dest: this.path, mark: this.el };
    return null;
  }

  onDragOver(e: DragEvent) {
    if (!app.dragging) return;
    const d = this.dropDest(e);
    if (!d) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "link";
    const cls = d.mark === this.el ? "drop-ok" : "drop-target";
    if (!d.mark.classList.contains(cls)) {
      clearDropMarks();
      d.mark.classList.add(cls);
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

// ================================================================ panes & cross-pane behaviour

const panes = [new Pane(0, $("#pane-0"), "source"), new Pane(1, $("#pane-1"), "dest")];

function setActive(id: number) {
  app.active = id;
  panes.forEach((p) => {
    p.el.classList.toggle("active", p.id === id);
    if (p.view === "drives") p.paintDriveFocus(false);
  });
}

/** key()s of the real folders behind the links selected on the left. */
function selectedLinkTargets(): Set<string> {
  return new Set(
    panes[0]
      .selectedEntries()
      .filter((e) => e.link?.target && e.link.targetExists)
      .map((e) => key(e.link!.target!)),
  );
}

/** Clicking a link on the left shows where the real folder lives on the right. */
function revealLinkTarget(entry: Entry) {
  if (!app.followLinks || !entry.link?.target) return;
  if (!entry.link.targetExists) {
    setStatus(`Broken link: ${entry.link.target} does not exist.`, true);
    return;
  }
  const t = entry.link.target;
  const par = parentOf(t) ?? t;
  const right = panes[1];
  if (right.view === "dir" && right.path && key(right.path) === key(par)) {
    right.paintMarks();
    right.flash(t);
  } else right.navigate(par, { highlight: t });
  setStatus(`${entry.name} is a ${KIND_LABEL[entry.link.kind].toLowerCase()} → ${t}`);
}

function sourcePaths(): string[] {
  return panes[0].view === "drives" ? [] : [...panes[0].selected];
}

/** Destination is always the right pane's current folder (or a folder dropped onto). */
function destPath(): string {
  const r = panes[1];
  return r.view === "dir" ? (r.path ?? "") : "";
}

/** The selected items on the left, if every one of them is an intact link — then the rail shows "Unlink". */
function unlinkable(): Entry[] | null {
  const left = panes[0];
  const sel = left.selectedEntries();
  if (!sel.length) return null;
  const ok = sel.every((e) => {
    if (left.view === "links") return left.links.get(e.path)?.status === "ok";
    return !!e.link && (e.link.kind === "junction" || e.link.kind === "symlink") && e.link.targetExists;
  });
  return ok ? sel : null;
}

function paneChanged(p: Pane) {
  if (p.isSource || p.view === "dir") scheduleAnalysis();
  if (!p.isSource) p.paintMarks();
}

// ================================================================ link bar

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
  const picked = app.userPickedKind ? a.options.find((o) => o.kind === app.pickedKind && o.available) : undefined;
  app.kind = picked ? picked.kind : (a.recommended ?? (cur?.available ? cur.kind : null));
  renderLinkBar();
}

function chosenOption(): LinkOption | undefined {
  return app.analysis?.options.find((o) => o.kind === app.kind);
}

function renderLinkBar() {
  const a = app.analysis;
  if (!a) return;
  const noSel = sourcePaths().length === 0;

  $("#fs-info").textContent = noSel
    ? destPath() && a.dstFs
      ? `destination: ${a.dstFs}`
      : ""
    : a.srcFs || a.dstFs
      ? `${a.srcFs || "?"} → ${a.dstFs || "?"}${a.srcFs && a.dstFs ? (a.sameVolume ? " · same drive" : " · different drives") : ""}`
      : "";

  $("#kinds").innerHTML = a.options
    .map((o) => {
      const off = !noSel && !o.available;
      const cls = ["kind", o.kind === app.kind && !noSel && "on", off && "off"].filter(Boolean).join(" ");
      const badges = `${o.recommended && !noSel ? `<span class="rec" title="Recommended">${icons.star()}</span>` : ""}${o.needsAdmin ? `<span class="adm" title="Needs elevated rights">${icons.shield(12)}</span>` : ""}`;
      return `<button class="${cls}" data-kind="${o.kind}" title="${esc(off ? (o.reason ?? "") : o.short)}">
        <span class="k-ico">${kindIcon[o.kind]}</span>
        <span class="k-title">${esc(o.title)}${badges}</span>
        <span class="k-short">${esc(off ? (o.reason ?? o.short) : o.short)}</span>
      </button>`;
    })
    .join("");

  const modeDesc =
    app.mode === "move"
      ? "Moves the selected items into the destination and leaves a link at the original place — programs keep using the old path."
      : "Leaves the data where it is and creates a link inside the destination that points to it.";
  const opt = chosenOption();
  $("#mode-desc").innerHTML =
    esc(modeDesc) + (opt?.keepsSource ? `<br><span class="faint">${esc(opt.title)} always keeps the original and creates the new entry in the destination.</span>` : "");
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
  const arrow = icons.arrowRight(13);

  $("#relink-btn").classList.toggle("hidden", !relinkable());
  const un = unlinkable();
  btn.classList.toggle("unlink", !!un);
  $("#link-btn-label").textContent = un ? "Unlink" : "Link";
  $("#link-btn-ico").innerHTML = un ? icons.unlink(26) : icons.link(26);
  btn.title = un ? "Remove the selected links (Ctrl+Enter)" : "Create links (Ctrl+Enter)";
  if (un) {
    btn.disabled = false;
    hint.textContent = `${un.length} link${un.length > 1 ? "s" : ""} selected`;
    pv.innerHTML =
      un
        .slice(0, 3)
        .map((e) => `<div class="pv"><span><b>${esc(e.path)}</b></span>${arrow}<span>${esc(e.link?.target ?? "")}</span></div>`)
        .join("") + `<div class="faint">Selected items are links — <b>Unlink</b> moves the data back or just removes the link.</div>`;
    return;
  }

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

  const lines = sources.slice(0, 3).map((s, i) => {
    const t = a?.targets[i] ?? (dest ? `${dest.replace(/[\\/]$/, "")}${isWin() ? "\\" : "/"}${baseName(s)}` : "?");
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
    app.pickedKind = k;
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
$("#link-btn").addEventListener("click", () => primaryAction());
$("#relink-btn").addEventListener("click", () => relinkAction());
$("#swap-btn").addEventListener("click", () => swapPanes());

function primaryAction() {
  const un = unlinkable();
  if (un) openUnlinkDialog(un);
  else openLinkDialog(sourcePaths(), destPath());
}

/** The selected links on the left, if all of them can be re-linked (junctions / symbolic links with a live target). */
function relinkable(): Entry[] | null {
  const un = unlinkable();
  return un && un.every((e) => e.link?.kind === "junction" || e.link?.kind === "symlink") ? un : null;
}

function relinkAction() {
  const links = relinkable();
  if (!links) return setStatus("Select one or more junctions or symbolic links on the left first.", true);
  const dest = destPath();
  if (!dest) return setStatus("Open a folder on the right first — that is where the linked data will live.", true);
  openRelinkDialog(links, dest);
}

/** Swap what the two panes show (e.g. to link a folder back after removing its junction). */
async function swapPanes() {
  const [l, r] = panes;
  if (l.view === "links") return setStatus("Saved links can only be shown on the left — open a folder there first.", true);
  const [a, b] = [l.view === "dir" ? l.path : null, r.view === "dir" ? r.path : null];
  await Promise.all([l.navigate(b), r.navigate(a)]);
  setStatus("Swapped left and right.");
}

// Mouse "back" / "forward" buttons navigate the pane under the pointer.
window.addEventListener(
  "mouseup",
  (e) => {
    if (e.button !== 3 && e.button !== 4) return;
    e.preventDefault();
    if (modal.open) return;
    const el = (e.target as Element | null)?.closest?.<HTMLElement>(".pane");
    const p = panes.find((x) => x.el === el) ?? panes[app.active];
    setActive(p.id);
    if (e.button === 3) p.goBack();
    else p.goForward();
  },
  true,
);
// stop the webview from using those buttons for its own page history
window.addEventListener("mousedown", (e) => (e.button === 3 || e.button === 4) && e.preventDefault(), true);
window.addEventListener("auxclick", (e) => (e.button === 3 || e.button === 4) && e.preventDefault(), true);

// ================================================================ modal infrastructure

const modal = {
  open: false,
  running: false,
  onEnter: null as null | (() => void),
};

function showModal(html: string, onEnter: (() => void) | null = null) {
  modal.open = true;
  modal.onEnter = onEnter;
  $("#modal").classList.remove("hidden");
  $("#modal-box").innerHTML = html;
}

function closeModal() {
  if (modal.running) return;
  modal.open = false;
  modal.onEnter = null;
  $("#modal").classList.add("hidden");
}

$("#modal").addEventListener("mousedown", (e) => {
  if (e.target === $("#modal")) closeModal();
});

const head = (icon: string, title: string, sub = "", color = "") =>
  `<div class="m-head"><div class="m-ico"${color ? ` style="background:${color}"` : ""}>${icon}</div><div><h3>${esc(title)}</h3>${sub ? `<p>${esc(sub)}</p>` : ""}</div></div>`;

const PHASES: Record<string, string> = {
  scanning: "Measuring",
  copying: "Copying",
  verifying: "Verifying copy",
  moving: "Moving",
  linking: "Creating link",
  unlinking: "Removing link",
  cloning: "Cloning",
  cleaning: "Removing old copy",
  deleting: "Deleting",
};

/** Run a long operation with a progress view, then show results and refresh everything. */
async function runOp(title: string, call: () => Promise<ItemResult[]>, opts: { revealTarget?: boolean } = {}) {
  modal.running = true;
  showModal(`
    ${head(icons.link(22), title + "…", "Starting")}
    <div class="m-body"><div class="prog">
      <div class="prog-bar indet" id="p-bar"><span></span></div>
      <div class="prog-meta"><span id="p-phase">Preparing</span><span id="p-bytes"></span></div>
      <div class="prog-file" id="p-file"></div>
    </div></div>
    <div class="m-foot"><span class="faint" style="margin-right:auto">Please keep Smart Symlink open until this finishes.</span></div>`);
  const sub = $("#modal-box .m-head p");
  const unlisten = await api.onProgress((p: Progress) => {
    sub.textContent = `${p.itemCount > 1 ? `(${p.itemIndex + 1}/${p.itemCount}) ` : ""}${p.item}`;
    $("#p-phase").textContent = PHASES[p.phase] ?? p.phase;
    const det = p.bytesTotal > 0 && (p.phase === "copying" || p.phase === "cloning");
    const bar = $("#p-bar");
    bar.classList.toggle("indet", !det);
    ($("span", bar) as HTMLElement).style.width = det ? `${((p.bytesDone / p.bytesTotal) * 100).toFixed(1)}%` : "";
    $("#p-bytes").textContent = p.bytesTotal ? `${fmtBytes(p.bytesDone)} / ${fmtBytes(p.bytesTotal)}` : "";
    $("#p-file").textContent = p.file ?? "";
  });

  let results: ItemResult[];
  try {
    results = await call();
  } catch (err) {
    results = [{ source: "", target: "", ok: false, message: String(err) }];
  }
  unlisten();
  modal.running = false;
  showResults(title, results, opts.revealTarget ?? false);
  panes.forEach((p) => p.selected.clear());
  await reloadStore();
  await Promise.all(panes.map((p) => p.refresh()));
}

function showResults(title: string, results: ItemResult[], revealTarget: boolean) {
  const ok = results.filter((r) => r.ok).length;
  const bad = results.length - ok;
  const list = results
    .map(
      (r) => `<div class="res ${r.ok ? "ok" : "bad"}"><span class="ri">${r.ok ? icons.check(12) : icons.x(12)}</span>
      <div class="rt"><b>${esc(baseName(r.source || r.target || title))}</b><span>${esc(r.message)}</span></div></div>`,
    )
    .join("");
  const first = results.find((r) => r.ok);
  const needsAdmin = isWin() && results.some((r) => !r.ok && /administrator/i.test(r.message));
  showModal(
    `${head(bad ? icons.warn(22) : icons.check(22), bad ? (ok ? "Partly done" : "Failed") : "Done", `${ok} succeeded${bad ? `, ${bad} failed` : ""}`, bad ? "var(--danger)" : "var(--ok)")}
    <div class="m-body"><div class="m-items">${list}</div></div>
    <div class="m-foot">${needsAdmin ? `<button class="btn" id="m-admin" style="margin-right:auto">${icons.shield(14)} Restart as administrator</button>` : ""}${first && revealTarget ? `<button class="btn" id="m-reveal">Show in ${fmName()}</button>` : ""}<button class="btn primary" id="m-close">Close</button></div>`,
    closeModal,
  );
  if (needsAdmin) $("#m-admin").onclick = () => api.restartElevated().catch((e) => setStatus(String(e), true));
  $("#m-close").onclick = closeModal;
  if (first && revealTarget) $("#m-reveal").onclick = () => api.reveal(first.target || first.source).catch((e) => setStatus(String(e), true));
  setStatus(bad ? `${title}: ${bad} failed.` : `${title}: ${ok} done.`, bad > 0);
}

/** Small "enter a name" dialog. `submit` returns an error message or null. */
function promptName(title: string, icon: string, initial: string, okLabel: string, note: string, submit: (name: string) => Promise<string | null>) {
  showModal(`
    ${head(icon, title)}
    <div class="m-body">
      <div class="field"><label>Name</label><input type="text" id="p-name" spellcheck="false" value="${esc(initial)}" /></div>
      ${note}
      <div class="err-line" id="p-err"></div>
    </div>
    <div class="m-foot"><button class="btn" id="m-cancel">Cancel</button><button class="btn primary" id="m-ok">${esc(okLabel)}</button></div>`);
  const input = $<HTMLInputElement>("#p-name");
  input.focus();
  const dot = initial.lastIndexOf(".");
  input.setSelectionRange(0, dot > 0 ? dot : initial.length); // select name without extension
  const go = async () => {
    const err = await submit(input.value.trim());
    if (err) $("#p-err").textContent = err;
    else closeModal();
  };
  modal.onEnter = go;
  $("#m-ok").onclick = go;
  $("#m-cancel").onclick = closeModal;
}

/** Shown in every dialog that may copy data between drives. */
function fidelityBlock(): string {
  return `<div class="note">${icons.shield(15)}<span>Every date, attribute, owner and permission is kept exactly and checked after copying. If anything can't be kept, the operation stops and nothing is changed.${app.privNote ? " " + esc(app.privNote) : ""}</span></div>
    <label class="check"><input type="checkbox" id="v-check" ${app.verifyContents ? "checked" : ""} /> Also compare the contents of every file after copying (slower)</label>`;
}

/** Read (and remember) the "compare contents" checkbox of the open dialog. */
function readVerify(): boolean {
  const c = document.getElementById("v-check") as HTMLInputElement | null;
  if (!c) return false;
  app.verifyContents = c.checked;
  savePref("verifyContents", c.checked);
  return c.checked;
}

// ================================================================ dialogs: link / unlink

const dlg = {
  sources: [] as string[],
  dest: "",
  mode: "move" as Mode,
  kind: null as LinkKind | null,
  analysis: null as Analysis | null,
};

async function openLinkDialog(sources: string[], dest: string) {
  if (!sources.length) return setStatus("Select one or more items on the left first.", true);
  if (!dest) return setStatus("Open a destination folder on the right first.", true);
  Object.assign(dlg, { sources, dest, mode: app.mode, kind: app.userPickedKind ? app.pickedKind : null });
  await refreshLinkDialog();
}

async function refreshLinkDialog() {
  dlg.analysis = await api.linkOptions(dlg.sources, dlg.dest, dlg.mode);
  const a = dlg.analysis;
  if (!a.options.find((o) => o.kind === dlg.kind && o.available)) dlg.kind = a.recommended;
  renderLinkDialog();
}

function renderLinkDialog() {
  const a = dlg.analysis!;
  const opt = a.options.find((o) => o.kind === dlg.kind);
  const moving = dlg.mode === "move" && !opt?.keepsSource;
  const n = dlg.sources.length;
  const arrow = icons.arrowRight(16);
  const P = (p: string) => `<span class="pp" title="${esc(p)}"><bdi>${esc(p)}</bdi></span>`;
  const items = dlg.sources
    .map((s, i) => {
      const t = a.targets[i] ?? "";
      if (moving)
        return `<div class="m-item"><div class="p">${P(s)}<small>becomes a ${esc(opt?.title.toLowerCase() ?? "link")}</small></div>${arrow}<div class="p">${P(t)}<small>data moves here</small></div></div>`;
      if (opt?.keepsSource)
        return `<div class="m-item"><div class="p">${P(s)}<small>stays as is</small></div>${arrow}<div class="p">${P(t)}<small>new ${esc(opt.title.toLowerCase())}</small></div></div>`;
      return `<div class="m-item"><div class="p">${P(t)}<small>new ${esc(opt?.title.toLowerCase() ?? "link")}</small></div>${arrow}<div class="p">${P(s)}<small>data stays here</small></div></div>`;
    })
    .join("");
  const chips = a.options
    .map(
      (o) =>
        `<button class="chip ${o.kind === dlg.kind ? "on" : ""}" data-kind="${o.kind}" ${o.available ? "" : "disabled"} title="${esc(o.available ? o.short : (o.reason ?? ""))}">${o.recommended ? icons.star(12) : ""}${esc(o.title)}</button>`,
    )
    .join("");
  const notes = [
    ...a.blocking.map((b) => `<div class="note err">${icons.warn(15)}<span>${esc(b)}</span></div>`),
    ...(moving ? a.warnings : []).map((w) => `<div class="note warn">${icons.warn(15)}<span>${esc(w)}</span></div>`),
    opt ? `<div class="note">${kindIcon[opt.kind]}<span><b>${esc(opt.title)}:</b> ${esc(opt.short)} ${esc(opt.useWhen)}</span></div>` : "",
  ].join("");
  const canStart = !!opt?.available && a.blocking.length === 0;

  showModal(
    `${head(icons.link(22), moving ? "Move & link back" : opt?.keepsSource ? `Create ${opt.title.toLowerCase()}` : "Create links", `${n} item${n > 1 ? "s" : ""} → ${dlg.dest}`)}
    <div class="m-body">
      <div class="seg" id="m-mode"><button data-mode="move" class="${dlg.mode === "move" ? "on" : ""}">Move &amp; link back</button><button data-mode="link" class="${dlg.mode === "link" ? "on" : ""}">Link only</button></div>
      <div class="m-kinds" id="m-kinds">${chips}</div>
      <div class="m-items">${items}</div>
      ${notes}
      ${moving && !a.sameVolume ? fidelityBlock() : ""}
    </div>
    <div class="m-foot"><button class="btn" id="m-cancel">Cancel</button><button class="btn primary" id="m-start" ${canStart ? "" : "disabled"}>${moving ? "Move & link" : "Create"}</button></div>`,
    canStart ? startLink : null,
  );
  $("#m-cancel").onclick = closeModal;
  $("#m-start").onclick = startLink;
  $("#m-mode").onclick = (e) => {
    const m = (e.target as HTMLElement).closest<HTMLElement>("[data-mode]")?.dataset.mode as Mode | undefined;
    if (m && m !== dlg.mode) {
      dlg.mode = m;
      refreshLinkDialog();
    }
  };
  $("#m-kinds").onclick = (e) => {
    const k = (e.target as HTMLElement).closest<HTMLElement>("[data-kind]")?.dataset.kind as LinkKind | undefined;
    if (k) {
      dlg.kind = k;
      renderLinkDialog();
    }
  };
}

function startLink() {
  const a = dlg.analysis;
  const opt = a?.options.find((o) => o.kind === dlg.kind);
  if (!a || !dlg.kind || !opt?.available || a.blocking.length) return;
  app.mode = dlg.mode;
  app.kind = dlg.kind;
  app.pickedKind = dlg.kind;
  app.userPickedKind = true;
  const req = { sources: dlg.sources, dest: dlg.dest, kind: dlg.kind, mode: dlg.mode, verify: readVerify() };
  runOp(dlg.mode === "move" && !opt.keepsSource ? "Move & link" : "Create links", () => api.execute(req), { revealTarget: true });
}

function openUnlinkDialog(entries: Entry[]) {
  const arrow = icons.arrowRight(16);
  const P = (p: string) => `<span class="pp" title="${esc(p)}"><bdi>${esc(p)}</bdi></span>`;
  const items = entries
    .map(
      (e) =>
        `<div class="m-item"><div class="p">${P(e.path)}<small>${esc(e.link ? KIND_LABEL[e.link.kind].toLowerCase() : "link")}</small></div>${arrow}<div class="p">${P(e.link?.target ?? "")}<small>real data</small></div></div>`,
    )
    .join("");
  const onlyHard = entries.every((e) => e.link?.kind === "hardlink");
  showModal(
    `${head(icons.unlink(22), "Unlink", `${entries.length} link${entries.length > 1 ? "s" : ""}`, "linear-gradient(135deg,#0fb5a2,#0e7c86)")}
    <div class="m-body">
      <div class="m-items">${items}</div>
      <div class="opts">
        ${
          onlyHard
            ? ""
            : `<label class="opt"><input type="radio" name="um" value="restore" checked /><b>Move the data back and remove the link</b>
          <span>Undo the move: the real folder returns to where the link is now. If anything fails, the link is put back automatically.</span></label>`
        }
        <label class="opt"><input type="radio" name="um" value="remove" ${onlyHard ? "checked" : ""} /><b>Remove only the link</b>
          <span>The data stays where it is (at the target); only the link disappears.</span></label>
      </div>
      ${onlyHard ? "" : fidelityBlock()}
    </div>
    <div class="m-foot"><button class="btn" id="m-cancel">Cancel</button><button class="btn primary" id="m-start">Unlink</button></div>`,
    () => $("#m-start").click(),
  );
  $("#m-cancel").onclick = closeModal;
  $("#m-start").onclick = () => {
    const mode = ($<HTMLInputElement>('input[name="um"]:checked').value as UnlinkMode) ?? "restore";
    const verify = readVerify();
    runOp("Unlink", () => api.unlink(entries.map((e) => e.path), mode, verify));
  };
}

const rdlg = {
  links: [] as Entry[],
  dest: "",
  kind: null as LinkKind | null,
  analysis: null as Analysis | null,
};

/** Re-link: keep the link where it is, move its data into `dest` and/or change the link type. */
async function openRelinkDialog(links: Entry[], dest: string) {
  Object.assign(rdlg, { links, dest, kind: null, analysis: null });
  try {
    rdlg.analysis = await api.relinkOptions(links.map((e) => e.path), dest);
  } catch (e) {
    return setStatus(String(e), true);
  }
  const a = rdlg.analysis!;
  // default: keep the type the links have now, so the first choice is "just move the data"
  const kinds = new Set(links.map((e) => e.link!.kind));
  const current = kinds.size === 1 ? [...kinds][0] : null;
  rdlg.kind = a.options.find((o) => o.kind === current && o.available)?.kind ?? a.recommended;
  renderRelinkDialog();
}

function renderRelinkDialog() {
  const a = rdlg.analysis!;
  const opt = a.options.find((o) => o.kind === rdlg.kind);
  const arrow = icons.arrowRight(16);
  const P = (p: string) => `<span class="pp" title="${esc(p)}"><bdi>${esc(p)}</bdi></span>`;
  const same = (x: string, y: string) => (isWin() ? x.toLowerCase() === y.toLowerCase() : x === y);
  let changes = 0;
  let moves = 0;
  const items = rdlg.links
    .map((e, i) => {
      const cur = e.link?.target ?? "";
      const next = a.targets[i] ?? cur;
      const moved = !same(cur, next);
      const retyped = !!rdlg.kind && e.link?.kind !== rdlg.kind;
      if (moved || retyped) changes++;
      if (moved) moves++;
      const was = KIND_LABEL[e.link!.kind].toLowerCase();
      const linkNote = retyped ? `${esc(was)} becomes a ${esc(opt?.title.toLowerCase() ?? "link")}` : `stays a ${esc(was)} here`;
      const dataNote = moved ? `data moves here from ${esc(cur)}` : "data stays here";
      return `<div class="m-item"><div class="p">${P(e.path)}<small>${linkNote}</small></div>${arrow}<div class="p">${P(next)}<small>${dataNote}</small></div></div>`;
    })
    .join("");
  const chips = a.options
    .map(
      (o) =>
        `<button class="chip ${o.kind === rdlg.kind ? "on" : ""}" data-kind="${o.kind}" ${o.available ? "" : "disabled"} title="${esc(o.available ? o.short : (o.reason ?? ""))}">${esc(o.title)}</button>`,
    )
    .join("");
  const nothing = a.blocking.length === 0 && changes === 0;
  const notes = [
    ...a.blocking.map((b) => `<div class="note err">${icons.warn(15)}<span>${esc(b)}</span></div>`),
    nothing
      ? `<div class="note warn">${icons.warn(15)}<span>Nothing would change: the data is already in this folder and the link type is the same. Open another folder on the right, or pick a different link type.</span></div>`
      : "",
    ...(a.blocking.length || nothing ? [] : a.warnings).map((w) => `<div class="note warn">${icons.warn(15)}<span>${esc(w)}</span></div>`),
    `<div class="note">${icons.relink(15)}<span>The link stays where it is, so programs keep using the same path. If anything fails, the data and the old link are put back.</span></div>`,
  ].join("");
  const canStart = !!opt?.available && a.blocking.length === 0 && changes > 0;
  const n = rdlg.links.length;

  showModal(
    `${head(icons.relink(22), "Re-link", `${n} link${n > 1 ? "s" : ""} → ${rdlg.dest}`, "linear-gradient(135deg,#0fb5a2,#4f8cff)")}
    <div class="m-body">
      <div class="m-kinds" id="m-kinds">${chips}</div>
      <div class="m-items">${items}</div>
      ${notes}
      ${moves > 0 && !a.sameVolume && !a.blocking.length ? fidelityBlock() : ""}
    </div>
    <div class="m-foot"><button class="btn" id="m-cancel">Cancel</button><button class="btn primary" id="m-start" ${canStart ? "" : "disabled"}>${moves > 0 ? "Move & re-link" : "Re-link"}</button></div>`,
    canStart ? startRelink : null,
  );
  $("#m-cancel").onclick = closeModal;
  $("#m-start").onclick = startRelink;
  $("#m-kinds").onclick = (e) => {
    const k = (e.target as HTMLElement).closest<HTMLElement>("[data-kind]")?.dataset.kind as LinkKind | undefined;
    if (k) {
      rdlg.kind = k;
      renderRelinkDialog();
    }
  };
}

function startRelink() {
  const a = rdlg.analysis;
  if (!a || !rdlg.kind || a.blocking.length || !a.options.find((o) => o.kind === rdlg.kind)?.available) return;
  const req = { links: rdlg.links.map((e) => e.path), dest: rdlg.dest, kind: rdlg.kind, verify: readVerify() };
  runOp("Re-link", () => api.relink(req), { revealTarget: true });
}

// ================================================================ dialogs: file operations

async function linkWarning(paths: string[], verb: string): Promise<string> {
  const hits = (await Promise.all(paths.map((p) => api.linksInto(p).catch(() => [])))).flat();
  if (!hits.length) return "";
  return `<div class="note warn">${icons.warn(15)}<span>${hits.length} saved link${hits.length > 1 ? "s point" : " points"} into ${paths.length > 1 ? "these items" : "this item"} (e.g. ${esc(hits[0].link)}). ${esc(verb)} will break ${hits.length > 1 ? "them" : "it"}.</span></div>`;
}

function newFolder(p: Pane) {
  if (p.view !== "dir" || !p.path) return;
  const parent = p.path;
  promptName("New folder", icons.folderPlus(22), "New folder", "Create", "", async (name) => {
    try {
      const made = await api.createDir(parent, name);
      await p.navigate(parent, { push: false, highlight: made });
      return null;
    } catch (e) {
      return String(e);
    }
  });
}

async function renameItem(p: Pane, e: Entry) {
  const warn = e.link ? "" : await linkWarning([e.path], "Renaming");
  promptName("Rename", icons.rename(22), e.name, "Rename", warn, async (name) => {
    if (name === e.name) return null;
    try {
      const newPath = await api.renameItem(e.path, name);
      await reloadStore();
      await p.navigate(p.loc, { push: false, highlight: newPath });
      return null;
    } catch (err) {
      return String(err);
    }
  });
}

async function deleteItems(items: Entry[]) {
  const links = items.filter((e) => e.link && e.link.kind !== "hardlink");
  const real = items.filter((e) => !links.includes(e));
  const warn = await linkWarning(
    real.map((e) => e.path),
    "Deleting",
  );
  const list = items
    .slice(0, 8)
    .map((e) => `<div class="m-item" style="grid-template-columns:1fr"><div class="p"><span class="pp"><bdi>${esc(e.path)}</bdi></span><small>${e.link && e.link.kind !== "hardlink" ? "link only — the target is not touched" : e.isDir ? "folder and everything in it" : "file"}</small></div></div>`)
    .join("");
  showModal(
    `${head(icons.trash(22), `Delete ${items.length} item${items.length > 1 ? "s" : ""}?`, "", "var(--danger)")}
    <div class="m-body">
      <div class="m-items">${list}${items.length > 8 ? `<div class="faint">… and ${items.length - 8} more</div>` : ""}</div>
      ${warn}
      ${real.length ? `<label class="check"><input type="checkbox" id="d-perm" /> Delete permanently (skip the ${isWin() ? "Recycle Bin" : "trash"})</label>` : ""}
    </div>
    <div class="m-foot"><button class="btn" id="m-cancel">Cancel</button><button class="btn danger" id="m-del">Delete</button></div>`,
  );
  $("#m-cancel").onclick = closeModal;
  $("#m-del").onclick = () => {
    const permanent = !!$<HTMLInputElement>("#d-perm")?.checked;
    runOp("Delete", () => api.deleteItems(items.map((e) => e.path), permanent));
  };
}

async function moveItems(items: Entry[], dest: string) {
  const warn = await linkWarning(
    items.filter((e) => !e.link).map((e) => e.path),
    "Moving",
  );
  const list = items
    .slice(0, 8)
    .map((e) => `<div class="m-item" style="grid-template-columns:1fr"><div class="p"><span class="pp"><bdi>${esc(e.path)}</bdi></span></div></div>`)
    .join("");
  showModal(
    `${head(icons.move(22), `Move ${items.length} item${items.length > 1 ? "s" : ""}`, `to ${dest}`)}
    <div class="m-body"><div class="m-items">${list}</div>${warn}
      <div class="note">${icons.info(15)}<span>A plain move — no link is left behind. Between drives the data is copied, verified, and only then removed.</span></div>
      ${fidelityBlock()}</div>
    <div class="m-foot"><button class="btn" id="m-cancel">Cancel</button><button class="btn primary" id="m-mv">Move</button></div>`,
    () => $("#m-mv").click(),
  );
  $("#m-cancel").onclick = closeModal;
  $("#m-mv").onclick = () => {
    const verify = readVerify();
    runOp("Move", () => api.moveItems(items.map((e) => e.path), dest, verify));
  };
}

async function showProperties(path: string) {
  try {
    if (await api.nativeProperties(path)) return;
  } catch {
    /* fall back to our own */
  }
  const e = await api.entryInfo(path).catch((err) => {
    setStatus(String(err), true);
    return null;
  });
  if (!e) return;
  const row = (k: string, v: string) => (v ? `<dt>${esc(k)}</dt><dd>${v}</dd>` : "");
  showModal(
    `${head(icons.info(22), e.name)}
    <div class="m-body"><dl class="props">
      ${row("Location", esc(parentOf(e.path) ?? e.path))}
      ${row("Type", esc(typeLabel(e)))}
      ${row("Size", e.isDir ? `<span id="pr-size" class="shimmer"></span>` : fmtBytes(e.size ?? 0))}
      ${row("Modified", e.modified ? dateFmt.format(e.modified) : "")}
      ${row("Points to", e.link?.target ? esc(e.link.target) + (e.link.targetExists ? "" : " (missing)") : "")}
      ${row("Hard links", e.link?.kind === "hardlink" ? String(e.link.linkCount) : "")}
      ${row("Hidden", e.hidden || e.dot ? "yes" : "")}
    </dl></div>
    <div class="m-foot"><button class="btn" id="m-reveal">Show in ${fmName()}</button><button class="btn primary" id="m-close">Close</button></div>`,
    closeModal,
  );
  $("#m-close").onclick = closeModal;
  $("#m-reveal").onclick = () => api.reveal(e.path);
  if (e.isDir && !e.link)
    api
      .dirSize(e.path, 1)
      .then((s) => {
        const el = document.getElementById("pr-size");
        if (el) el.outerHTML = `${fmtBytes(s.bytes)} <span class="faint">(${s.files.toLocaleString()} files, ${s.dirs.toLocaleString()} folders)</span>`;
      })
      .catch(() => {});
}

async function toggleFavorite(path: string, on: boolean) {
  try {
    app.favorites = await api.setFavorite(path, on);
  } catch (e) {
    return setStatus(String(e), true);
  }
  setStatus(on ? `Added ${baseName(path)} to Quick Access.` : `Removed ${baseName(path)} from Quick Access.`);
  panes.forEach((p) => p.render());
}

function showHelp() {
  const k = (...keys: string[]) => keys.map((x) => `<kbd>${esc(x)}</kbd>`).join(" ");
  const rows = (r: [string, string][]) => `<table>${r.map(([a, b]) => `<tr><td>${a}</td><td>${esc(b)}</td></tr>`).join("")}</table>`;
  showModal(
    `${head(icons.keyboard(22), "Keyboard & mouse")}
    <div class="m-body"><div class="help">
      <div>
        <h4>Moving around</h4>
        ${rows([
          [`${k("↑", "↓")} or ${k("j", "k")}`, "Move up / down"],
          [`${k("Enter")} ${k("→")} ${k("l")}`, "Open folder"],
          [`${k("Backspace")} ${k("←")} ${k("h")}`, "Up one level"],
          [`${k("Alt", "←")} / ${k("Alt", "→")}`, "Back / forward"],
          [k("Tab"), "Switch between left and right"],
          [`${k("Home")} ${k("End")} ${k("PgUp")} ${k("PgDn")}`, "Jump"],
          [k("Ctrl", "L"), "Type a path"],
        ])}
        <h4 style="margin-top:16px">This PC (drives)</h4>
        ${rows([
          [`${k("←", "↑", "↓", "→")} or ${k("h", "j", "k", "l")}`, "Move between drives"],
          [`${k("Enter")} ${k("Space")}`, "Open drive"],
        ])}
      </div>
      <div>
        <h4>Selecting (left side)</h4>
        ${rows([
          [k("Space"), "Select / unselect"],
          [`${k("Shift", "↑↓")}`, "Extend selection"],
          [k("Ctrl", "A"), "Select all"],
          [k("Esc"), "Clear selection"],
        ])}
        <h4 style="margin-top:16px">Actions</h4>
        ${rows([
          [k("Ctrl", "Enter"), "Link — or Unlink when links are selected"],
          [k("Ctrl", "R"), "Re-link: move a link's data or change its type"],
          [k("Ctrl", "U"), "Swap left and right"],
          [k("F2"), "Rename"],
          [k("Del"), "Delete"],
          [k("Ctrl", "Shift", "N"), "New folder"],
          [k("F5"), "Refresh"],
          [k("?"), "This help"],
        ])}
        <h4 style="margin-top:16px">Mouse & touch</h4>
        ${rows([
          ["Double-click / tap", "Open"],
          ["Click · Ctrl+click · Shift+click", "Select on the left"],
          ["Drag left → right", "Link into that folder"],
          ["Right-click", "Rename, delete, move, properties…"],
          ["Mouse back / forward buttons", "Back / forward in the pane under the pointer"],
          ["☆ next to a folder", "Pin to Quick Access"],
          ["Click a link (left)", "Shows its real folder on the right"],
        ])}
      </div>
    </div></div>
    <div class="m-foot"><button class="btn primary" id="m-close">Got it</button></div>`,
    closeModal,
  );
  $("#m-close").onclick = closeModal;
}

// ================================================================ context menus

type MenuItem = { label: string; icon: string; run: () => void; hint?: string; danger?: boolean; disabled?: boolean } | "-" | { head: string };

function openMenu(items: MenuItem[], e: MouseEvent) {
  const el = $("#ctx");
  el.innerHTML = items
    .map((it, i) => {
      if (it === "-") return "<hr>";
      if ("head" in it) return `<div class="ctx-head">${esc(it.head)}</div>`;
      return `<button data-i="${i}" class="${it.danger ? "danger" : ""}" ${it.disabled ? "disabled" : ""}>${it.icon}<span>${esc(it.label)}</span>${it.hint ? `<span class="hint">${esc(it.hint)}</span>` : ""}</button>`;
    })
    .join("");
  el.classList.remove("hidden");
  const r = el.getBoundingClientRect();
  el.style.left = `${Math.min(e.clientX, window.innerWidth - r.width - 8)}px`;
  el.style.top = `${Math.min(e.clientY, window.innerHeight - r.height - 8)}px`;
  el.onclick = (ev) => {
    const b = (ev.target as HTMLElement).closest<HTMLElement>("button[data-i]");
    if (!b) return;
    const it = items[Number(b.dataset.i)];
    closeMenu();
    if (typeof it === "object" && "run" in it) it.run();
  };
}

function closeMenu() {
  $("#ctx").classList.add("hidden");
}
document.addEventListener("mousedown", (e) => {
  if (!(e.target as HTMLElement).closest("#ctx")) closeMenu();
});
window.addEventListener("blur", closeMenu);

function otherPane(p: Pane) {
  return panes[1 - p.id];
}

function itemMenu(p: Pane, items: Entry[], _e: MouseEvent) {
  if (!items.length) return;
  const one = items.length === 1 ? items[0] : null;
  const other = otherPane(p);
  const moveDest = other.view === "dir" ? other.path : null;
  const un = p.isSource ? unlinkable() : one?.link && one.link.kind !== "hardlink" && one.link.targetExists ? [one] : null;
  const saved = p.view === "links" ? items.map((i) => p.links.get(i.path)).filter(Boolean) : [];
  const m: MenuItem[] = [{ head: one ? one.name : `${items.length} items` }];

  if (one?.isDir && p.view === "dir") m.push({ label: "Open", icon: icons.open(), hint: "Enter", run: () => p.open(one) });
  if (p.view === "links" && one) m.push({ label: "Go to link location", icon: icons.open(), run: () => p.open(one) });
  if (one?.link?.target && one.link.targetExists) m.push({ label: "Show link target", icon: icons.target(16), run: () => revealLinkTarget(one) });
  if (un) m.push({ label: un.length > 1 ? `Unlink ${un.length} links…` : "Unlink…", icon: icons.unlink(16), hint: p.isSource ? "Ctrl+Enter" : "", run: () => openUnlinkDialog(un) });
  if (p.isSource && relinkable()) m.push({ label: "Re-link…", icon: icons.relink(16), hint: "Ctrl+R", run: () => relinkAction() });
  if (p.view === "links" && saved.length)
    m.push({
      label: saved.length > 1 ? "Forget these entries" : "Forget this entry",
      icon: icons.x(16),
      run: async () => {
        for (const s of saved) await api.forgetLink(s!.saved.link);
        await reloadStore();
        p.refresh();
      },
    });
  m.push("-");
  if (one?.isDir && p.view === "dir")
    m.push({ label: isFav(one.path) ? "Remove from Quick Access" : "Add to Quick Access", icon: isFav(one.path) ? icons.star(16) : icons.starOutline(16), run: () => toggleFavorite(one.path, !isFav(one.path)) });
  if (one && p.view === "dir") m.push({ label: "Rename…", icon: icons.rename(), hint: "F2", run: () => renameItem(p, one) });
  if (p.view === "dir")
    m.push({
      label: moveDest ? `Move to “${baseName(moveDest)}”…` : "Move to other side…",
      icon: icons.move(),
      disabled: !moveDest,
      hint: moveDest ? "" : "open a folder there first",
      run: () => moveItems(items, moveDest!),
    });
  if (p.view === "dir") m.push({ label: "Delete…", icon: icons.trash(), hint: "Del", danger: true, run: () => deleteItems(items) });
  m.push("-");
  if (one) {
    m.push({ label: `Show in ${fmName()}`, icon: icons.reveal(), run: () => api.reveal(one.path).catch((err) => setStatus(String(err), true)) });
    m.push({ label: "Copy path", icon: icons.copy(), run: () => navigator.clipboard.writeText(one.path).then(() => setStatus("Path copied.")) });
    m.push({ label: "Properties", icon: icons.info(), run: () => showProperties(one.path) });
  } else m.push({ label: "Copy paths", icon: icons.copy(), run: () => navigator.clipboard.writeText(items.map((i) => i.path).join("\n")).then(() => setStatus("Paths copied.")) });
  openMenu(m, _e);
}

function folderMenu(p: Pane, e: MouseEvent) {
  const path = p.path!;
  openMenu(
    [
      { head: baseName(path) },
      { label: "New folder…", icon: icons.folderPlus(16), hint: "Ctrl+Shift+N", run: () => newFolder(p) },
      { label: "Refresh", icon: icons.refresh(), hint: "F5", run: () => p.refresh() },
      "-",
      { label: isFav(path) ? "Remove this folder from Quick Access" : "Add this folder to Quick Access", icon: icons.starOutline(16), run: () => toggleFavorite(path, !isFav(path)) },
      { label: `Open in ${fmName()}`, icon: icons.reveal(), run: () => api.reveal(path) },
      { label: "Copy path", icon: icons.copy(), run: () => navigator.clipboard.writeText(path).then(() => setStatus("Path copied.")) },
      { label: "Properties", icon: icons.info(), run: () => showProperties(path) },
    ],
    e,
  );
}

function driveMenu(p: Pane, card: HTMLElement, e: MouseEvent) {
  const path = card.dataset.mount!;
  if (path === LINKS) return;
  const fav = !!card.dataset.fav;
  openMenu(
    [
      { head: path },
      { label: "Open", icon: icons.open(), run: () => p.navigate(path) },
      ...(fav ? [{ label: "Remove from Quick Access", icon: icons.star(16), run: () => toggleFavorite(path, false) }] : []),
      "-",
      { label: `Open in ${fmName()}`, icon: icons.reveal(), run: () => api.reveal(path) },
      { label: "Properties", icon: icons.info(), run: () => showProperties(path) },
    ],
    e,
  );
}

// ================================================================ keyboard

document.addEventListener("keydown", (e) => {
  if (modal.open) {
    if (e.key === "Escape") closeModal();
    else if (e.key === "Enter" && !modal.running && modal.onEnter && !(e.target as Element).matches?.("button")) {
      e.preventDefault();
      modal.onEnter();
    }
    return;
  }
  if (!$("#ctx").classList.contains("hidden")) {
    if (e.key === "Escape") closeMenu();
    return;
  }
  if ((e.target as HTMLElement).tagName === "INPUT") return;
  const p = panes[app.active];
  const k = e.key;
  const ctrl = e.ctrlKey || e.metaKey;

  if (k === "?" || (k === "F1" && !ctrl)) return void (e.preventDefault(), showHelp());
  if (k === "Tab") return void (e.preventDefault(), setActive(1 - app.active));
  if (k === "Enter" && ctrl) return void primaryAction();
  if (ctrl && !e.shiftKey && k.toLowerCase() === "r") return void (e.preventDefault(), relinkAction());
  if (ctrl && k.toLowerCase() === "u") return void (e.preventDefault(), swapPanes());
  if (k === "F5") return void (e.preventDefault(), p.refresh());
  if (ctrl && k.toLowerCase() === "l") return void (e.preventDefault(), p.editPath());
  if (ctrl && e.shiftKey && k.toLowerCase() === "n") return void (e.preventDefault(), newFolder(p));
  if (k === "ArrowLeft" && e.altKey) return void p.goBack();
  if (k === "ArrowRight" && e.altKey) return void p.goForward();

  // drives: 2-D movement
  if (p.view === "drives") {
    const dirs: Record<string, [number, number]> = {
      ArrowLeft: [-1, 0], h: [-1, 0], ArrowRight: [1, 0], l: [1, 0], ArrowUp: [0, -1], k: [0, -1], ArrowDown: [0, 1], j: [0, 1],
    };
    if (dirs[k]) {
      e.preventDefault();
      p.moveDriveFocus(...dirs[k]);
    } else if (k === "Enter" || k === " ") {
      e.preventDefault();
      p.driveCards()[p.driveFocus]?.click();
    }
    return;
  }

  if (k === "ArrowDown" || k === "j") return void (e.preventDefault(), p.moveFocus(1, e.shiftKey));
  if (k === "ArrowUp" || k === "k") return void (e.preventDefault(), p.moveFocus(-1, e.shiftKey));
  if (k === "PageDown") return void p.moveFocus(15, e.shiftKey);
  if (k === "PageUp") return void p.moveFocus(-15, e.shiftKey);
  if (k === "Home") return void p.moveFocus(-1e9, e.shiftKey);
  if (k === "End") return void p.moveFocus(1e9, e.shiftKey);
  if (k === "Enter" || k === "l" || k === "ArrowRight") {
    const f = p.focusedEntry();
    if (f) p.open(f);
    return;
  }
  if (k === "Backspace" || k === "h" || k === "ArrowLeft") return void (e.preventDefault(), p.up());
  if (k === " " && p.isSource) {
    e.preventDefault();
    const f = p.focusedEntry();
    if (f) {
      p.toggle(f.path);
      p.selectionChanged();
      p.moveFocus(1, false);
    }
    return;
  }
  if (ctrl && k.toLowerCase() === "a" && p.isSource) {
    e.preventDefault();
    p.selected = new Set(p.rows.map((r) => r.path));
    return void p.selectionChanged();
  }
  if (k === "Escape" && p.isSource) {
    p.selected.clear();
    return void p.selectionChanged();
  }
  if (k === "F2" && p.view === "dir") {
    e.preventDefault();
    const it = p.actionItems();
    if (it.length === 1) renameItem(p, it[0]);
    return;
  }
  if (k === "Delete" && p.view === "dir") {
    const it = p.actionItems();
    if (it.length) deleteItems(it);
  }
});

document.addEventListener("contextmenu", (e) => {
  if ((e.target as HTMLElement).tagName !== "INPUT") e.preventDefault();
});

// ================================================================ header controls

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
const filterChanged = () =>
  panes.forEach((p) => {
    p.computeRows();
    p.render();
    p.startSizes();
  });
$<HTMLInputElement>("#opt-hidden").addEventListener("change", (e) => {
  app.showHidden = (e.target as HTMLInputElement).checked;
  savePref("showHidden", app.showHidden);
  filterChanged();
});
$<HTMLInputElement>("#opt-dot").addEventListener("change", (e) => {
  app.showDot = (e.target as HTMLInputElement).checked;
  savePref("showDot", app.showDot);
  filterChanged();
});
$<HTMLInputElement>("#opt-follow").addEventListener("change", (e) => {
  app.followLinks = (e.target as HTMLInputElement).checked;
});
$("#saved-btn").addEventListener("click", () => panes[0].navigate(LINKS));
$("#help-btn").addEventListener("click", showHelp);
$("#keys-hint").addEventListener("click", showHelp);

function savePref(k: string, v: unknown) {
  try {
    localStorage.setItem(k, JSON.stringify(v));
  } catch {
    /* ignore */
  }
}
function loadPref<T>(k: string, d: T): T {
  try {
    const v = localStorage.getItem(k);
    return v == null ? d : (JSON.parse(v) as T);
  } catch {
    return d;
  }
}

const THEMES: Theme[] = ["system", "light", "dark"];
function applyTheme(t: Theme) {
  app.theme = t;
  if (t === "system") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = t;
  try {
    localStorage.setItem("theme", t);
  } catch {
    /* ignore */
  }
  const btn = $("#theme-btn");
  btn.innerHTML = t === "light" ? icons.sun() : t === "dark" ? icons.moon() : icons.auto();
  btn.title = `Theme: ${t === "system" ? "follow system" : t} (click to change)`;
}
$("#theme-btn").addEventListener("click", () => applyTheme(THEMES[(THEMES.indexOf(app.theme) + 1) % THEMES.length]));

// ================================================================ start

(async function init() {
  $("#brand-logo").innerHTML = icons.link(22);
  $("#link-btn-ico").innerHTML = icons.link(26);
  $("#relink-ico").innerHTML = icons.relink(18);
  $("#swap-ico").innerHTML = icons.swap(18);
  $("#saved-ico").innerHTML = icons.list(16);
  $("#help-btn").innerHTML = icons.keyboard();
  try {
    applyTheme((localStorage.getItem("theme") as Theme) || "system");
  } catch {
    applyTheme("system");
  }
  app.showHidden = loadPref("showHidden", false);
  app.showDot = loadPref("showDot", false);
  app.verifyContents = loadPref("verifyContents", false);
  $<HTMLInputElement>("#opt-hidden").checked = app.showHidden;
  $<HTMLInputElement>("#opt-dot").checked = app.showDot;
  try {
    app.platform = await api.platform();
    app.home = await api.homeDir();
  } catch {
    /* running outside Tauri (plain browser) */
  }
  if (!isWin()) $("#opt-hidden-wrap").classList.add("hidden");
  app.privNote = await api.privilegesNote().catch(() => null);
  await reloadStore();
  setActive(0);
  await Promise.all(panes.map((p) => p.navigate(null)));
  scheduleAnalysis();
  await sleep(0);
})();
