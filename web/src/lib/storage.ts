import type { HostPayloads } from './protocol';
export const DOWNLOAD_LIMIT = 65536;
export function appendFileChunk(
  file: HostPayloads['file'],
  bytes: number[],
  chunk: HostPayloads['file_data'],
): boolean {
  if (
    chunk.index !== file.index ||
    chunk.offset !== bytes.length ||
    chunk.total_size !== file.size ||
    chunk.data.length > Math.min(128, DOWNLOAD_LIMIT - bytes.length, file.size - bytes.length) ||
    (!chunk.eof && chunk.data.length === 0) ||
    (chunk.eof && chunk.offset + chunk.data.length !== file.size)
  )
    throw new Error('Invalid file response');
  bytes.push(...chunk.data);
  return chunk.eof || bytes.length >= Math.min(file.size, DOWNLOAD_LIMIT);
}
