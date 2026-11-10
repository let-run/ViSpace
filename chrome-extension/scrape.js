/* Runs only after the user invokes the extension on an LCSC product page. */
function scrapeLCSC() {
  if (!/(^|\.)lcsc\.com$/i.test(location.hostname) || !/\/product-detail\//i.test(location.pathname)) throw new Error('Open an LCSC product detail page first.');
  const clean = value => String(value || '').replace(/\s+/g, ' ').trim();
  const meta = key => document.querySelector(`meta[property="${key}"],meta[name="${key}"]`)?.content || '';
  const absolute = value => { if (!value) return ''; try { const url = new URL(value, location.href); return ['http:', 'https:'].includes(url.protocol) ? url.href : ''; } catch { return ''; } };
  const products = [];
  function walk(value) {
    if (!value || typeof value !== 'object') return;
    if ([value['@type']].flat().includes('Product')) products.push(value);
    for (const child of Object.values(value)) if (child && typeof child === 'object') Array.isArray(child) ? child.forEach(walk) : walk(child);
  }
  for (const script of document.querySelectorAll('script[type="application/ld+json"]')) { try { walk(JSON.parse(script.textContent)); } catch {} }
  const product = products[0] || {};
  const pairs = new Map();
  for (const row of document.querySelectorAll('tr, [class*="parameter-item"], [class*="attribute-item"], [class*="info-item"]')) {
    const cells = [...row.children].map(el => clean(el.innerText)).filter(Boolean);
    if (cells.length >= 2) pairs.set(cells[0].replace(/:$/, '').toLowerCase(), cells.slice(1).join(' '));
  }
  // The product information panel also uses adjacent label/value elements.
  const lines = (document.body.innerText || '').split('\n').map(clean).filter(Boolean);
  const lookup = (...labels) => {
    for (const label of labels) {
      if (pairs.has(label.toLowerCase())) return pairs.get(label.toLowerCase());
      const index = lines.findIndex(line => line.replace(/:$/, '').toLowerCase() === label.toLowerCase());
      if (index >= 0) return lines[index + 1] || '';
    }
    return '';
  };
  // Keep visible quantity tiers together so a bulk price is never presented as a single-unit price.
  const priceRows = [...document.querySelectorAll('tr')].map(row => {
    const text = clean(row.innerText || row.textContent);
    return /(?:US\s*\$|USD|[$€£¥])\s*\d/.test(text) && /^\d[\d,]*(?:\s*(?:\+|[-–~]\s*\d[\d,]*))?\s/.test(text) ? text : '';
  }).filter(Boolean);
  const offers = [product.offers].flat().filter(Boolean);
  const offer = offers.find(value => value.price !== undefined) || offers[0] || {};
  const amount = offer.price ?? offer.priceSpecification?.price;
  const currency = clean(offer.priceCurrency || offer.priceSpecification?.priceCurrency || meta('product:price:currency'));
  const metadataPrice = meta('product:price:amount');
  const visiblePrice = [...document.querySelectorAll('[class*="price"], [class*="Price"]')]
    .map(el => clean(el.innerText)).find(text => /^(?:(?:US\s*\$|USD|[$€£¥])\s*\d[\d,.]*|\d[\d,.]*\s*(?:USD|EUR|GBP|CNY))(?:\s*\/\s*(?:pc|pcs|piece))?$/i.test(text)) || '';
  const singleUnitPrice = amount !== undefined ? clean(`${currency} ${amount} / unit`) : metadataPrice ? clean(`${currency} ${metadataPrice} / unit`) : visiblePrice;
  const price = priceRows.length ? [...new Set(priceRows)].join('; ') :
    amount !== undefined ? clean(`${currency} ${amount} / unit`) :
    metadataPrice ? clean(`${currency} ${metadataPrice} / unit`) : visiblePrice;
  const lcsc = clean(product.sku).match(/^C\d+$/i)?.[0] || location.pathname.match(/_?(C\d+)\.html/i)?.[1] || lookup('LCSC Part #', 'LCSC Part Number').match(/C\d+/i)?.[0] || '';
  const image = [product.image].flat()[0];
  const datasheet = [...document.querySelectorAll('a[href]')].find(a => /datasheet/i.test(a.textContent) && /\.pdf(?:[?#]|$)/i.test(a.href)) || [...document.querySelectorAll('a[href]')].find(a => /\.pdf(?:[?#]|$)/i.test(a.href));
  return {supplier: 'LCSC', spn: lcsc, price, singleUnitPrice, number: clean(product.mpn || lookup('Manufacturer Part Number', 'Mfr. Part #', 'Mfr Part #') || document.querySelector('h1')?.innerText || product.name), manufacturer: clean(product.brand?.name || product.manufacturer?.name || lookup('Manufacturer', 'Brand')), lcsc, description: clean(product.description || meta('og:description') || meta('description')), package: lookup('Package', 'Package / Case', 'Packaging'), value: lookup('Value'), tolerance: lookup('Tolerance'), datasheet: absolute(datasheet?.href || ''), image: absolute((typeof image === 'object' ? image?.url : image) || meta('og:image')), source: location.href};
}
