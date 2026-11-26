import * as Collapsible from '@kobalte/core/collapsible';
import { createUniqueId, For, Show } from 'solid-js';
import type { HostPayloads } from '../lib/protocol';
import { DOWNLOAD_LIMIT } from '../lib/storage';
import { Button } from './ui/button';

function EntryIcon(props: { directory: boolean }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      class="size-4 shrink-0"
    >
      <Show
        when={props.directory}
        fallback={
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8l-6-6Zm0 0v6h6" />
        }
      >
        <path d="M3 7V5a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7Z" />
      </Show>
    </svg>
  );
}

/** Root-only file browser: the host does not expose subdirectory contents. */
export function StorageFileTree(props: {
  files: readonly HostPayloads['file'][];
  disabled: boolean;
  downloadingFile: number | null;
  onDownload: (file: HostPayloads['file']) => void;
}) {
  const id = createUniqueId();
  return (
    <Collapsible.Root defaultOpen class="mt-3 min-w-0 rounded-md border p-2">
      <Collapsible.Trigger
        as={Button}
        variant="ghost"
        size="sm"
        class="group w-full justify-start"
        aria-label="FAT32 root"
      >
        <svg
          aria-hidden="true"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          class="transition-transform group-data-expanded:rotate-90"
        >
          <path d="m9 6 6 6-6 6" />
        </svg>
        <EntryIcon directory />
        <span class="font-mono">/</span>
        <span class="text-muted-foreground">FAT32 root</span>
      </Collapsible.Trigger>
      <Collapsible.Content>
        <ul aria-label="Root files" class="ml-4 min-w-0 border-l pl-2">
          <For
            each={props.files}
            fallback={
              <li class="px-3 py-3 text-sm text-muted-foreground">
                Select List files to read the root directory.
              </li>
            }
          >
            {(file) => (
              <li class="flex min-w-0 items-center gap-2">
                <Button
                  variant="ghost"
                  size="sm"
                  class="h-auto min-h-9 min-w-0 flex-1 justify-start whitespace-normal py-2 text-left"
                  aria-describedby={`${id}-${file.index}`}
                  aria-label={
                    file.directory ? `Folder ${file.name}, unavailable` : `Download ${file.name}`
                  }
                  title={
                    file.directory
                      ? 'Subfolders cannot be opened'
                      : file.size > DOWNLOAD_LIMIT
                        ? 'Files above 64 KiB cannot be downloaded'
                        : undefined
                  }
                  loading={props.downloadingFile === file.index}
                  disabled={props.disabled || file.directory || file.size > DOWNLOAD_LIMIT}
                  onClick={() => props.onDownload(file)}
                >
                  <EntryIcon directory={file.directory} />
                  <span class="min-w-0 flex-1 break-all font-mono">{file.name}</span>
                </Button>
                <span id={`${id}-${file.index}`} class="text-right text-xs text-muted-foreground">
                  {file.directory
                    ? 'Folder · unavailable'
                    : file.size > DOWNLOAD_LIMIT
                      ? 'Above 64 KiB'
                      : `${file.size.toLocaleString()} bytes`}
                </span>
              </li>
            )}
          </For>
        </ul>
      </Collapsible.Content>
    </Collapsible.Root>
  );
}
