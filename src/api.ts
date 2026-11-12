import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type LinkKind = "junction" | "symlink" | "hardlink" | "clone" | "bind" | "snapshot";
export type Mode = "move" | "link";

export interface DriveInfo {
  name: string;
  mount: string;
  fs: string;
  total: number;
  free: number;
  removable: boolean;
  kind: string;
}

export interface LinkInfo {
  kind: LinkKind;
  target: string | null;
  targetExists: boolean;
  linkCount: number | null;
}

export interface Entry {
  name: string;
  path: string;
  isDir: boolean;
  size: number | null;
  modified: number | null;
  hidden: boolean;
  link: LinkInfo | null;
}

export interface Crumb {
  name: string;
  path: string;
}

export interface Listing {
  path: string;
  parent: string | null;
  crumbs: Crumb[];
  entries: Entry[];
  fs: string;
  free: number | null;
  total: number | null;
}

export interface SizeInfo {
  bytes: number;
  files: number;
  dirs: number;
  errors: number;
}

export interface LinkOption {
  kind: LinkKind;
  title: string;
  short: string;
  details: string;
  useWhen: string;
  available: boolean;
  reason: string | null;
  needsAdmin: boolean;
  recommended: boolean;
  keepsSource: boolean;
}

export interface Analysis {
  options: LinkOption[];
  srcFs: string;
  dstFs: string;
  sameVolume: boolean;
  allDirs: boolean;
  allFiles: boolean;
  recommended: LinkKind | null;
  blocking: string[];
  warnings: string[];
  targets: string[];
}

export interface Progress {
  itemIndex: number;
  itemCount: number;
  item: string;
  phase: string;
  file: string | null;
  bytesDone: number;
  bytesTotal: number;
}

export interface ItemResult {
  source: string;
  target: string;
  ok: boolean;
  message: string;
}

export const api = {
  platform: () => invoke<string>("platform"),
  homeDir: () => invoke<string | null>("home_dir"),
  listDrives: () => invoke<DriveInfo[]>("list_drives"),
  listDir: (path: string) => invoke<Listing>("list_dir", { path }),
  dirSize: (path: string, pane: number) => invoke<SizeInfo>("dir_size", { path, pane }),
  cancelSizes: (pane: number) => invoke<void>("cancel_sizes", { pane }),
  invalidateSizes: (path: string | null) => invoke<void>("invalidate_sizes", { path }),
  linkOptions: (sources: string[], dest: string, mode: Mode) =>
    invoke<Analysis>("link_options", { sources, dest, mode }),
  execute: (req: { sources: string[]; dest: string; kind: LinkKind; mode: Mode }) =>
    invoke<ItemResult[]>("execute", { req }),
  reveal: (path: string) => invoke<void>("reveal", { path }),
  onProgress: (cb: (p: Progress) => void): Promise<UnlistenFn> =>
    listen<Progress>("op-progress", (e) => cb(e.payload)),
};
