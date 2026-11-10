/* Self-contained because Chrome serializes this function into the active tab. */
function scrapeMouser() {
  const pageError = 'Open a Mouser product detail page first.';
  if (!/\/ProductDetail\//i.test(location.pathname)) throw Error(pageError);

  const clean = value => String(value ?? '').replace(/\s+/g, ' ').trim();
  const text = node => clean(node?.innerText || node?.textContent);
  const meta = key => document.querySelector(`meta[property="${key}"]`)?.content || '';
  const absolute = value => {
    if (!value) return '';
    const url = URL.parse(value, location.href);
    return url && /^https?:$/.test(url.protocol) ? url.href : '';
  };
  const products = [];
  function findProducts(value) {
    if (!value || typeof value !== 'object') return;
    if ([value['@type']].flat().includes('Product')) products.push(value);
    Object.values(value).forEach(findProducts);
  }
  for (const script of document.querySelectorAll('script[type="application/ld+json"]')) {
    try { findProducts(JSON.parse(script.textContent)); } catch {}
  }
  const product = products[0] || {};
  const labels = new Map();
  const labelKey = value => clean(value).replace(/[:：]$/, '').trim().toLowerCase();
  const remember = (label, value) => {
    const key = labelKey(label);
    if (value && !labels.has(key)) labels.set(key, value);
  };
  for (const row of document.querySelectorAll('tr')) {
    const [label, value] = [...row.querySelectorAll('th, td')].map(text);
    if (label) remember(label, value);
  }
  const lines = (document.body?.innerText || '').split('\n').map(clean).filter(Boolean);
  for (const [i, line] of lines.entries()) {
    const pair = line.match(/^([^:：]+)[:：]\s*(.*)$/);
    if (pair) remember(pair[1], pair[2] || lines[i + 1]);
  }
  const lookup = (...keys) => keys.map(key => labels.get(labelKey(key))).find(Boolean) || '';
  const number = text(document.querySelector('#spnManufacturerPartNum, [itemprop="mpn"]')) ||
    lookup('Mfr. #', 'Mfr. No', 'Mfr. Part #', 'Manufacturer Part Number') || clean(product.mpn);
  const spn = text(document.querySelector('#spnMouserPartNum, [itemprop="sku"]')) ||
    lookup('Mouser #', 'Mouser No', 'Mouser Part Number') || clean(product.sku);
  if (!number && !spn) throw Error('Could not find product details on this Mouser page.');

  // Read unit prices by column heading so the extended total is never imported.
  const unitPrice = /^(?:US\s*\$|USD|EUR|GBP|CNY|[$€£¥])\s*\d+(?:,\d{3})*(?:\.\d+)?$/i;
  const pricing = [...document.querySelectorAll('table')].map(table => {
    const headings = [...table.querySelectorAll('th')].map(text);
    const quantity = headings.findIndex(value => /^(?:quantity|qty\.?|qty\.\s*price break)$/i.test(value));
    const price = headings.findIndex(value => /^unit price$/i.test(value));
    if (quantity < 0 || price < 0) return [];
    return [...table.querySelectorAll('tr')].flatMap(row => {
      const cells = [...row.querySelectorAll('td')].map(text);
      const minimum = cells[quantity]?.match(/^(\d[\d,]*)\s*[:+]?$/)?.[1];
      return minimum && unitPrice.test(cells[price]) ? [`${minimum}+ ${cells[price]}`] : [];
    });
  }).filter(rows => rows.length);
  const tiers = pricing.find(rows => rows.some(row => row.startsWith('1+ '))) || pricing[0] || [];
  const offer = [product.offers].flat().find(value => value?.price !== undefined);
  const scalar = offer ? clean(`${offer.priceCurrency || ''} ${offer.price}`) : '';
  const singleUnitPrice = tiers.find(row => row.startsWith('1+ '))?.slice(3) ||
    (unitPrice.test(scalar) ? `${scalar} / unit` : '');
  const image = [product.image].flat()[0];
  const imageUrl = typeof image === 'object' ? image?.url || image?.contentUrl : image;
  const datasheet = [...document.querySelectorAll('a[href]')].find(link =>
    /datasheet/i.test(text(link) || link.title || '') &&
    /\/datasheet\/|\.pdf(?:[?#]|$)/i.test(link.href));
  return {
    supplier: 'Mouser', number, spn,
    manufacturer: text(document.querySelector('#lnkManufacturer')) || lookup('Manufacturer', 'Mfr.') ||
      clean(product.brand?.name || product.manufacturer?.name),
    description: lookup('Description', 'Product Description') || clean(product.description || meta('og:description')),
    package: lookup('Package / Case', 'Package/Case', 'Package Case'),
    value: lookup('Resistance', 'Capacitance', 'Inductance'),
    tolerance: lookup('Tolerance'),
    datasheet: absolute(datasheet?.href),
    image: absolute(imageUrl || meta('og:image') || document.querySelector('img#imgProduct, img[itemprop="image"]')?.src),
    price: tiers.join('; ') || singleUnitPrice,
    singleUnitPrice,
    source: location.href,
  };
}
