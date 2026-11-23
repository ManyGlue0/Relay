import { useEffect, useState } from "react";
import { api, type FolderInfo } from "./api";
import { useStore } from "./store";

/** The last part of a path. */
export function folderName(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || path;
}

const known = new Map<string, FolderInfo>();

/** A session folder's name, whether it's there and its git branch: asked
 *  again each time the window comes back to the front (a branch switched in
 *  a terminal shows up). */
export function useFolderInfo(path: string | null | undefined): FolderInfo | null {
  const active = useStore((s) => s.windowActive);
  const [info, setInfo] = useState<FolderInfo | null>(() => (path ? (known.get(path) ?? null) : null));
  useEffect(() => {
    if (!path) {
      setInfo(null);
      return;
    }
    setInfo(known.get(path) ?? null);
    if (!active) return;
    let live = true;
    void api
      .folderInfo(path)
      .then((i) => {
        known.set(path, i);
        if (live) setInfo(i);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [path, active]);
  return info;
}
