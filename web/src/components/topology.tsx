import { For, Show } from 'solid-js';
import { hex, type ProtocolState } from '../lib/protocol';
import { Panel } from './panel';
import { Table, TableHeader, TableBody, TableRow, TableHead, TableCell } from './ui/table';

function DescriptorTable(props: {
  caption: string;
  headers: string[];
  rows: (string | number)[][];
}) {
  return (
    <Table>
      <caption class="sr-only">{props.caption}</caption>
      <TableHeader>
        <TableRow>
          <For each={props.headers}>{(header) => <TableHead scope="col">{header}</TableHead>}</For>
        </TableRow>
      </TableHeader>
      <TableBody>
        <For
          each={props.rows}
          fallback={
            <TableRow>
              <TableCell colSpan={props.headers.length} class="empty">
                No details reported yet
              </TableCell>
            </TableRow>
          }
        >
          {(row) => (
            <TableRow>
              <For each={row}>
                {(cell) => <TableCell class="font-mono text-xs">{cell}</TableCell>}
              </For>
            </TableRow>
          )}
        </For>
      </TableBody>
    </Table>
  );
}
export function Topology(props: { state: ProtocolState }) {
  return (
    <Panel title="USB device">
      <details>
        <summary>Device details</summary>
        <h3>Device descriptor</h3>
        <Show
          when={props.state.device}
          fallback={
            <p class="empty">No device details yet. Connect a USB device and start the host.</p>
          }
        >
          {(device) => (
            <dl class="descriptor-grid">
              <For each={Object.entries(device())}>
                {([key, value]) => (
                  <div>
                    <dt>{key.replaceAll('_', ' ')}</dt>
                    <dd>
                      {hex(
                        value,
                        ['vendor_id', 'product_id', 'usb_version', 'device_version'].includes(key)
                          ? 4
                          : 2,
                      )}
                    </dd>
                  </div>
                )}
              </For>
            </dl>
          )}
        </Show>
        <h3 class="mt-6">Interfaces</h3>
        <DescriptorTable
          caption="USB interfaces"
          headers={['Number / alt', 'Class / sub / protocol', 'Endpoints']}
          rows={props.state.interfaces.map((d) => [
            `${d.number} / ${d.alternate}`,
            `${hex(d.class)} / ${hex(d.subclass)} / ${hex(d.protocol)}`,
            d.endpoints,
          ])}
        />
        <For each={props.state.interfaces}>
          {(face) => (
            <>
              <h3 class="mt-6">
                Interface {face.number} · alternate {face.alternate}
              </h3>
              <DescriptorTable
                caption={`Interface ${face.number}, alternate ${face.alternate}: endpoints`}
                headers={['Address', 'Attributes', 'Max packet', 'Interval']}
                rows={props.state.endpoints
                  .filter((d) => d.interface === face.number && d.alternate === face.alternate)
                  .map((d) => [
                    `${hex(d.address)} ${d.address & 128 ? 'IN' : 'OUT'}`,
                    hex(d.attributes),
                    d.max_packet_size,
                    d.interval,
                  ])}
              />
            </>
          )}
        </For>
      </details>
    </Panel>
  );
}
export function Hid(props: { state: ProtocolState }) {
  const usages = (values: number[]) => values.map((value) => hex(value)).join(' ') || '—';
  return (
    <Panel title="Input activity" description="Keys are shown as USB codes, not characters.">
      <h3>Keyboard</h3>
      <Show when={props.state.keyboard} fallback={<p class="empty">No keyboard activity</p>}>
        {(keyboard) => (
          <dl class="readings">
            <div>
              <dt>Modifiers</dt>
              <dd>{hex(keyboard().modifiers)}</dd>
            </div>
            <div>
              <dt>Keys</dt>
              <dd>{usages(keyboard().keys)}</dd>
            </div>
            <div>
              <dt>Pressed</dt>
              <dd>{usages(keyboard().pressed)}</dd>
            </div>
            <div>
              <dt>Released</dt>
              <dd>{usages(keyboard().released)}</dd>
            </div>
          </dl>
        )}
      </Show>
      <h3 class="mt-6">Mouse</h3>
      <Show when={props.state.mouse} fallback={<p class="empty">No mouse activity</p>}>
        {(mouse) => (
          <dl class="readings">
            <div>
              <dt>Buttons</dt>
              <dd>{hex(mouse().buttons)}</dd>
            </div>
            <div>
              <dt>Δx / Δy</dt>
              <dd>
                {mouse().dx} / {mouse().dy}
              </dd>
            </div>
            <div>
              <dt>Wheel</dt>
              <dd>{mouse().wheel}</dd>
            </div>
          </dl>
        )}
      </Show>
    </Panel>
  );
}
