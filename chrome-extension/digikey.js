/* Self-contained because Chrome serializes this function into the active tab. */
function scrapeDigiKey() {
  if (!/(^|\.)digikey\.[a-z.]+$/i.test(location.hostname) || !/\/products\/detail\//i.test(location.pathname)) throw new Error('Open a DigiKey product detail page first.');
  const clean = value => String(value || '').replace(/\s+/g, ' ').trim();
  const absolute = value => { if (!value) return ''; try { const url = new URL(value, location.href); return /^https?:$/.test(url.protocol) ? url.href : ''; } catch { return ''; } };
  const meta = key => document.querySelector(`meta[property="${key}"],meta[name="${key}"]`)?.content || '';
  const products = [];
  function walk(value) {
    if (!value || typeof value !== 'object') return;
    if ([value['@type']].flat().includes('Product')) products.push(value);
    for (const child of Object.values(value)) if (child && typeof child === 'object') Array.isArray(child) ? child.forEach(walk) : walk(child);
  }
  for (const script of document.querySelectorAll('script[type="application/ld+json"]')) { try { walk(JSON.parse(script.textContent)); } catch {} }
  const product = products[0] || {};
  const pairs = new Map();
  for (const row of document.querySelectorAll('tr')) {
    const cells = [...row.querySelectorAll('th,td')].map(el => clean(el.innerText || el.textContent));
    if (cells.length >= 2 && !pairs.has(cells[0].toLowerCase())) pairs.set(cells[0].replace(/:$/, '').toLowerCase(), cells.slice(1).join(' '));
  }
  const lines = (document.body.innerText || '').split('\n').map(clean).filter(Boolean);
  const lookup = (...labels) => {
    for (const label of labels) {
      if (pairs.has(label.toLowerCase())) return pairs.get(label.toLowerCase());
      const index = lines.findIndex(line => line.replace(/:$/, '').toLowerCase() === label.toLowerCase());
      if (index >= 0) return lines[index + 1] || '';
    }
    return '';
  };
  const text = selector => clean(document.querySelector(selector)?.textContent);
  // Use one pricing table only: cut tape and reel prices must not be mixed.
  const tables = [...document.querySelectorAll('table')].map(table => [...table.querySelectorAll('tr')].map(row => {
    const cells = [...row.querySelectorAll('td')].map(el => clean(el.innerText || el.textContent));
    if (cells.length < 2 || !/^\d[\d,]*$/.test(cells[0]) || !/^(?:(?:US\s*\$|USD|EUR|GBP|CNY|[$€£¥])\s*\d)/i.test(cells[1])) return '';
    return `${cells[0]}+ ${cells[1]}`;
  }).filter(Boolean)).filter(rows => rows.length);
  const rows = tables.find(rows => /^1\+ /.test(rows[0])) || tables[0] || [];
  const offers = [product.offers].flat().filter(Boolean);
  const offer = offers.find(value => value.price !== undefined) || {};
  const scalar = offer.price !== undefined ? clean(`${offer.priceCurrency || ''} ${offer.price} / unit`) : '';
  const singleUnitPrice = rows.find(row => /^1\+ /.test(row))?.replace(/^1\+ /, '') || scalar;
  const supplierNumbers = text('[data-testid="digi-key-part-number"]') || lookup('DigiKey Part Number', 'Digi-Key Part Number') || clean(product.sku);
  // Prefer the cut-tape SKU when the panel lists multiple packaging variants.
  const skus = supplierNumbers.match(/[A-Z0-9][A-Z0-9./+_-]*-ND\b/gi) || [];
  const spn = skus.find(value => /(?:-1|CT)-ND$/i.test(value)) || skus[0] || supplierNumbers;
  const image = [product.image].flat()[0];
  const datasheet = [...document.querySelectorAll('a[href]')].find(a => /datasheet/i.test(a.textContent) && !/digikey\.[^/]+\/en\/products\//i.test(a.href));
  return {supplier: 'DigiKey', spn,
    number: text('[data-testid="mfr-number"]') || lookup('Manufacturer Product Number', 'Manufacturer Part Number') || clean(product.mpn),
    manufacturer: text('[data-testid="manufacturer"]') || lookup('Manufacturer') || clean(product.brand?.name || product.manufacturer?.name),
    description: lookup('Detailed Description', 'Description') || clean(product.description || meta('og:description')),
    package: lookup('Supplier Device Package', 'Package / Case'), value: lookup('Resistance', 'Capacitance', 'Inductance'), tolerance: lookup('Tolerance'),
    datasheet: absolute(datasheet?.href), image: absolute((typeof image === 'object' ? image?.url : image) || meta('og:image')),
    price: rows.join('; ') || scalar, singleUnitPrice, source: location.href};
}
