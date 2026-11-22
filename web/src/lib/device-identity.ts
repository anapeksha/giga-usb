import type { InterfaceDescriptor, ProtocolState } from './protocol';

import { usbId } from './usb-ids';
function classLabel(face: Pick<InterfaceDescriptor, 'class' | 'subclass' | 'protocol'>): string {
  switch (face.class) {
    case 3:
      if (face.subclass === 1 && face.protocol === 1) return 'HID keyboard';
      if (face.subclass === 1 && face.protocol === 2) return 'HID mouse';
      return 'HID input device';
    case 8:
      return 'USB storage';
    case 9:
      return 'USB hub';
    case 1:
      return 'USB audio';
    case 14:
      return 'USB video';
    case 2:
    case 10:
      return 'USB communications';
    default:
      return 'USB device';
  }
}

function storageText(value: string, limit: number): string {
  return value
    .slice(0, limit)
    .replace(/\p{Cc}/gu, ' ')
    .trim();
}

export function deviceIdentity(state: Pick<ProtocolState, 'device' | 'interfaces' | 'storage'>) {
  if (!state.device) return null;
  const device = state.device;
  const labels = [
    ...new Set((state.interfaces.length ? state.interfaces : [device]).map(classLabel)),
  ];
  const kind = labels.join(' / ');
  const reported = state.storage
    ? [storageText(state.storage.vendor, 8), storageText(state.storage.product, 16)]
        .filter(Boolean)
        .join(' ')
    : '';
  if (reported) {
    return { label: `Reported: ${reported}`, provenance: 'Storage inquiry vendor / product' };
  }
  const identity = usbId(device.vendor_id, device.product_id);
  if (identity.product) {
    return {
      label: `Likely ${identity.vendor} ${identity.product}`,
      provenance: `Offline USB ID match · ${kind}`,
    };
  }
  return {
    label: `Likely ${identity.vendor ? `${identity.vendor} ` : ''}${kind}`,
    provenance: identity.vendor
      ? 'Offline vendor ID match and descriptor classes; model unknown'
      : 'Descriptor classes only; make and model unknown',
  };
}
