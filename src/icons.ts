// Small inline SVG icon set (stroke-based, inherits currentColor).
const s = (body: string, size = 18) =>
  `<svg viewBox="0 0 24 24" width="${size}" height="${size}" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">${body}</svg>`;

export const icons = {
  back: () => s(`<path d="M15 18l-6-6 6-6"/>`),
  forward: () => s(`<path d="M9 18l6-6-6-6"/>`),
  up: () => s(`<path d="M12 19V5"/><path d="M5 12l7-7 7 7"/>`),
  refresh: () => s(`<path d="M21 12a9 9 0 1 1-2.64-6.36"/><path d="M21 4v5h-5"/>`),
  computer: () =>
    s(`<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8"/><path d="M12 16v4"/>`),
  home: () => s(`<path d="M3 11l9-7 9 7"/><path d="M5 10v10h14V10"/>`),
  edit: () => s(`<path d="M4 20h4L19 9l-4-4L4 16v4z"/>`, 15),
  reveal: () => s(`<path d="M14 3h7v7"/><path d="M21 3l-9 9"/><path d="M19 14v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h5"/>`, 15),
  folder: (cls = "") =>
    `<svg class="${cls}" viewBox="0 0 24 24" width="20" height="20"><path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h4.38l2 2.2h8.62A1.5 1.5 0 0 1 21 8.7v9.8a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5z" fill="currentColor" opacity=".9"/><path d="M3 9h18" stroke="rgba(0,0,0,.18)" stroke-width="1"/></svg>`,
  folderLink: (cls = "") =>
    `<svg class="${cls}" viewBox="0 0 24 24" width="20" height="20"><path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h4.38l2 2.2h8.62A1.5 1.5 0 0 1 21 8.7v9.8a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5z" fill="currentColor" opacity=".9"/><path d="M10 16.5l4.5-4.5M11 11.5h3.5V15" stroke="var(--bg)" stroke-width="1.9" fill="none" stroke-linecap="round"/></svg>`,
  file: (cls = "") =>
    `<svg class="${cls}" viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M6 3h8l4 4v13a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z" fill="currentColor" fill-opacity=".12"/><path d="M14 3v4h4"/></svg>`,
  fileLink: (cls = "") =>
    `<svg class="${cls}" viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M6 3h8l4 4v13a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z" fill="currentColor" fill-opacity=".18"/><path d="M14 3v4h4"/><path d="M9 16l4-4M10 12h3v3" stroke-width="1.8"/></svg>`,
  drive: () =>
    `<svg viewBox="0 0 48 48" width="40" height="40"><rect x="5" y="12" width="38" height="24" rx="5" fill="currentColor" opacity=".18"/><rect x="5" y="12" width="38" height="24" rx="5" fill="none" stroke="currentColor" stroke-width="2.2"/><circle cx="35" cy="24" r="2.6" fill="currentColor"/><path d="M11 24h14" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"/></svg>`,
  usb: () =>
    `<svg viewBox="0 0 48 48" width="40" height="40" fill="none" stroke="currentColor" stroke-width="2.2"><rect x="14" y="16" width="20" height="26" rx="4" fill="currentColor" fill-opacity=".18"/><rect x="18" y="6" width="12" height="10" rx="1.5"/><path d="M21 10h1M26 10h1"/></svg>`,
  homeBig: () =>
    `<svg viewBox="0 0 48 48" width="40" height="40" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linejoin="round"><path d="M7 22L24 8l17 14" /><path d="M11 19v20h26V19" fill="currentColor" fill-opacity=".18"/><path d="M20 39V28h8v11"/></svg>`,
  link: (size = 26) =>
    s(`<path d="M10 14a4.5 4.5 0 0 0 6.4 0l3.2-3.2a4.5 4.5 0 0 0-6.4-6.4L12 5.6"/><path d="M14 10a4.5 4.5 0 0 0-6.4 0l-3.2 3.2a4.5 4.5 0 0 0 6.4 6.4l1.2-1.2"/>`, size),
  arrowRight: (size = 16) => s(`<path d="M5 12h14"/><path d="M13 6l6 6-6 6"/>`, size),
  check: (size = 14) => s(`<path d="M5 12l5 5 9-10"/>`, size),
  x: (size = 14) => s(`<path d="M6 6l12 12M18 6L6 18"/>`, size),
  warn: (size = 16) => s(`<path d="M12 3l10 18H2z"/><path d="M12 10v5M12 18v.5"/>`, size),
  shield: (size = 14) => s(`<path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z"/>`, size),
  unlink: (size = 26) =>
    s(`<path d="M10 14a4.5 4.5 0 0 0 6.4 0l3.2-3.2a4.5 4.5 0 0 0-6.4-6.4"/><path d="M14 10a4.5 4.5 0 0 0-6.4 0l-3.2 3.2a4.5 4.5 0 0 0 6.4 6.4"/><path d="M3 3l18 18"/>`, size),
  swap: (size = 18) => s(`<path d="M4 8h15"/><path d="M15 4l4 4-4 4"/><path d="M20 16H5"/><path d="M9 12l-4 4 4 4"/>`, size),
  relink: (size = 18) => s(`<path d="M20 11a8 8 0 0 0-14.500-4.300"/><path d="M5 3v4h4"/><path d="M4 13a8 8 0 0 0 14.500 4.300"/><path d="M19 21v-4h-4"/>`, size),
  folderPlus: (size = 22) =>
    s(`<path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h4.4l2 2.2h8.6A1.5 1.5 0 0 1 21 8.7v9.8a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5z"/><path d="M12 11v6M9 14h6"/>`, size),
  trash: (size = 16) => s(`<path d="M4 7h16"/><path d="M10 11v6M14 11v6"/><path d="M6 7l1 13h10l1-13"/><path d="M9 7V4h6v3"/>`, size),
  rename: (size = 16) => s(`<path d="M4 20h4L19 9l-4-4L4 16v4z"/><path d="M13 7l4 4"/>`, size),
  info: (size = 16) => s(`<circle cx="12" cy="12" r="9"/><path d="M12 11v6M12 7.5v.5"/>`, size),
  move: (size = 16) => s(`<path d="M4 12h13"/><path d="M13 7l5 5-5 5"/><path d="M21 5v14"/>`, size),
  open: (size = 16) => s(`<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v1"/><path d="M3 7v11a2 2 0 0 0 2 2h13l3-8H7l-4 8"/>`, size),
  copy: (size = 16) => s(`<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M4 16V5a1 1 0 0 1 1-1h11"/>`, size),
  list: (size = 18) => s(`<path d="M9 6h11M9 12h11M9 18h11"/><path d="M4 6h.01M4 12h.01M4 18h.01" stroke-width="3"/>`, size),
  sun: (size = 18) => s(`<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>`, size),
  moon: (size = 18) => s(`<path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z"/>`, size),
  auto: (size = 18) => s(`<circle cx="12" cy="12" r="9"/><path d="M12 3v18" /><path d="M12 3a9 9 0 0 1 0 18z" fill="currentColor"/>`, size),
  keyboard: (size = 18) => s(`<rect x="2" y="6" width="20" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>`, size),
  starOutline: (size = 14) => s(`<path d="M12 3l2.7 5.9 6.3.7-4.7 4.3 1.3 6.3L12 17l-5.6 3.2 1.3-6.3L3 9.6l6.3-.7z"/>`, size),
  target: (size = 13) => s(`<circle cx="12" cy="12" r="8"/><circle cx="12" cy="12" r="3" fill="currentColor"/>`, size),
  star: (size = 13) =>
    `<svg viewBox="0 0 24 24" width="${size}" height="${size}"><path d="M12 3l2.7 5.9 6.3.7-4.7 4.3 1.3 6.3L12 17l-5.6 3.2 1.3-6.3L3 9.6l6.3-.7z" fill="currentColor"/></svg>`,
};

export const kindIcon: Record<string, string> = {
  junction: s(`<path d="M4 7h7a4 4 0 0 1 4 4v9"/><path d="M11 16l4 4 4-4"/><circle cx="4" cy="7" r="1.6" fill="currentColor"/>`, 22),
  symlink: s(`<path d="M10 14a4.5 4.5 0 0 0 6.4 0l3.2-3.2a4.5 4.5 0 0 0-6.4-6.4L12 5.6"/><path d="M14 10a4.5 4.5 0 0 0-6.4 0l-3.2 3.2a4.5 4.5 0 0 0 6.4 6.4l1.2-1.2"/>`, 22),
  hardlink: s(`<rect x="3" y="8" width="8" height="10" rx="1.5"/><rect x="13" y="8" width="8" height="10" rx="1.5"/><path d="M7 8V5h10v3"/>`, 22),
  clone: s(`<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M4 16V5a1 1 0 0 1 1-1h11"/>`, 22),
  bind: s(`<path d="M4 12h16"/><circle cx="4" cy="12" r="2"/><circle cx="20" cy="12" r="2"/><path d="M8 7l-4 5 4 5M16 7l4 5-4 5"/>`, 22),
  snapshot: s(`<rect x="3" y="7" width="18" height="13" rx="2"/><circle cx="12" cy="13.5" r="3.5"/><path d="M8 7l1.5-3h5L16 7"/>`, 22),
};
