import { Action, ActionPanel, Icon, List, getPreferenceValues } from "@raycast/api";
import { useExec } from "@raycast/utils";
import { homedir } from "os";
import { useState } from "react";

/** One file as `lumen --json search|ask` prints it. */
interface Hit {
  path: string;
  name: string;
  parent: string;
  kind: string;
  size: number;
  mtime: number;
  offline: boolean;
  cloud?: boolean;
  snippet?: string;
  score?: number;
}

const KIND_ICON: Record<string, Icon> = {
  document: Icon.Document,
  pdf: Icon.Document,
  spreadsheet: Icon.BarChart,
  presentation: Icon.AppWindowGrid3x3,
  image: Icon.Image,
  video: Icon.Video,
  audio: Icon.Music,
  archive: Icon.Box,
  code: Icon.Code,
  ebook: Icon.Book,
  design: Icon.Brush,
};

function human(bytes: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${bytes} B` : `${v.toFixed(1)} ${units[i]}`;
}

function tilde(p: string): string {
  const home = homedir();
  return p.startsWith(home) ? "~" + p.slice(home.length) : p;
}

/** The search list shared by both commands; `mode` picks the CLI subcommand. */
export function LumenSearch({ mode }: { mode: "search" | "ask" }) {
  const { lumenPath } = getPreferenceValues<{ lumenPath?: string }>();
  const bin = lumenPath?.trim() || "/Applications/Lumen.app/Contents/MacOS/lumen";
  const [query, setQuery] = useState("");
  const { data, isLoading, error } = useExec(bin, ["--json", mode, query], {
    execute: query.trim().length > 0,
    keepPreviousData: true,
    parseOutput: ({ stdout }) => (stdout.trim() ? (JSON.parse(stdout).hits as Hit[]) : []),
  });

  const placeholder = mode === "ask" ? "Describe the file: 北海道的雪景视频, the lease I signed…" : "Name, pinyin (nzzj), folder or words inside…";
  return (
    <List isLoading={isLoading} onSearchTextChange={setQuery} searchBarPlaceholder={placeholder} throttle>
      {error ? (
        <List.EmptyView
          icon={Icon.ExclamationMark}
          title="Lumen isn't reachable"
          description={`Install Lumen in /Applications or set the lumen binary in preferences.\n${error.message}`}
        />
      ) : (
        <List.EmptyView icon={Icon.MagnifyingGlass} title={query ? "No matches" : "Type to search your Mac"} />
      )}
      {(query.trim() ? (data ?? []) : []).map((h) => (
        <List.Item
          key={h.path}
          icon={{ fileIcon: h.path }}
          title={h.name}
          subtitle={h.snippet ?? tilde(h.parent)}
          accessories={[
            ...(h.cloud ? [{ icon: Icon.Cloud, tooltip: "In iCloud, not downloaded" }] : []),
            ...(h.offline ? [{ icon: Icon.HardDrive, tooltip: "On a drive that is not connected" }] : []),
            ...(h.score !== undefined ? [{ text: `${Math.round(h.score * 100)}%` }] : []),
            { text: human(h.size), icon: KIND_ICON[h.kind] },
            { date: new Date(h.mtime * 1000) },
          ]}
          actions={
            <ActionPanel>
              <Action.Open title="Open" target={h.path} />
              <Action.ShowInFinder path={h.path} />
              <Action.ToggleQuickLook shortcut={{ modifiers: ["cmd"], key: "y" }} />
              <Action.CopyToClipboard title="Copy Path" content={h.path} shortcut={{ modifiers: ["cmd"], key: "c" }} />
              <Action.OpenWith path={h.path} shortcut={{ modifiers: ["cmd"], key: "o" }} />
            </ActionPanel>
          }
          quickLook={{ path: h.path, name: h.name }}
        />
      ))}
    </List>
  );
}
