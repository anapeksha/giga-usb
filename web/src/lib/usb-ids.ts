import source from './usb.ids?raw';

/** Only vendor/product records: class and interface IDs have separate namespaces. */
export function parseUsbIds(text: string) {
  const vendors = new Map<number, { name: string; products: Map<number, string> }>();
  let vendor: ReturnType<typeof vendors.get>;
  for (const line of text.split('\n')) {
    const match = /^(\t?)([\da-f]{4}) {2}(.+)$/i.exec(line);
    if (match) {
      const id = Number.parseInt(match[2], 16);
      if (!match[1]) {
        vendor = { name: match[3].trim(), products: new Map() };
        vendors.set(id, vendor);
      } else {
        vendor?.products.set(id, match[3].trim());
      }
    } else if (line && !line.startsWith('#') && !line.startsWith('\t')) {
      vendor = undefined;
    }
  }
  return vendors;
}

const vendors = parseUsbIds(source);

export function usbId(vendorId: number, productId: number) {
  const vendor = vendors.get(vendorId);
  return { vendor: vendor?.name, product: vendor?.products.get(productId) };
}
